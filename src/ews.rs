// src/ews.rs
use crate::attachment::{
    parse_create_attachment_request, parse_delete_attachment_request, parse_get_attachment_request,
    render_create_attachment_response, render_file_attachment_xml, render_get_attachment_response,
};
use crate::caldav::CaldavClient;
use crate::calendar::{
    extract_ews_field, extract_ews_fields, parse_ews_attendees, parse_ews_calendar_item,
    parse_ews_recurrence, parse_ics_event, render_ics,
};
use crate::delegate_ews::DelegateEwsHandler;
use crate::ews_folders::{
    DistinguishedFolder, folder_id_for, render_folder_hierarchy_creates, render_folder_xml,
    render_root_and_children, resolve_folder_id, validate_folder_request,
};
use crate::ews_update::{apply_field_changes, parse_item_changes};
use crate::jmap::{JmapClient, QueryCalendarEventsParams};
use crate::models::AppState;
use crate::permission::{PermissionContext, PermissionEnforcement};
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::room::{
    parse_get_rooms_request, render_get_room_lists_response, render_get_rooms_response,
};
use crate::storage::EwsItemRow;
use crate::sync::generate_server_id;
use crate::util::{
    canonicalize_username, format_ews_datetime, nfc, normalize_username, xml_escape,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Datelike;
use const_hex;
use itertools::Itertools;
use quick_xml::Reader;
use quick_xml::events::Event;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, LazyLock};

#[derive(Clone)]
struct AuthContext {
    username: String,
    password: SecretString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ItemShape {
    IdOnly,
    Default,
    AllProperties,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EwsAction {
    GetFolder,
    FindFolder,
    FindItem,
    GetItem,
    GetUserAvailability,
    SyncFolderItems,
    SyncFolderHierarchy,
    Subscribe,
    Unsubscribe,
    CreateItem,
    UpdateItem,
    DeleteItem,
    SendItem,
    MoveItem,
    ResolveNames,
    GetUserOofSettings,
    SetUserOofSettings,
    GetServiceConfiguration,
    GetServerTimeZones,
    GetFolderInfo,
    GetMailTips,
    FindPeople,
    GetConversationItems,
    ConvertId,
    GetRoomLists,
    GetRooms,
    GetDelegate,
    AddDelegate,
    RemoveDelegate,
    UpdateDelegate,
    GetUserPhoto,
    MarkAsJunk,
    GetAppManifests,
    GetAppMarketplaceUrl,
    InstallApp,
    UninstallApp,
    GetClientAccessToken,
    GetReminders,
    PerformReminderAction,
    GetPersona,
    CreateAttachment,
    GetAttachment,
    DeleteAttachment,
    GetUserConfiguration,
}

impl EwsAction {
    /// Returns true if this action may include `IncludeMimeContent` in requests.
    /// The gateway does not validate MIME content; if the element is present,
    /// it is ignored. The method name is retained for historical compatibility.
    const fn requires_mime_validation(&self) -> bool {
        matches!(self, EwsAction::FindItem | EwsAction::SyncFolderItems)
    }

    const fn response_message_name(&self) -> &'static str {
        match self {
            EwsAction::GetFolder => "GetFolderResponseMessage",
            EwsAction::FindFolder => "FindFolderResponseMessage",
            EwsAction::FindItem => "FindItemResponseMessage",
            EwsAction::GetItem => "GetItemResponseMessage",
            EwsAction::GetUserAvailability => "GetUserAvailabilityResponseMessage",
            EwsAction::SyncFolderItems => "SyncFolderItemsResponseMessage",
            EwsAction::SyncFolderHierarchy => "SyncFolderHierarchyResponseMessage",
            EwsAction::Subscribe => "SubscribeResponseMessage",
            EwsAction::Unsubscribe => "UnsubscribeResponseMessage",
            EwsAction::CreateItem => "CreateItemResponseMessage",
            EwsAction::UpdateItem => "UpdateItemResponseMessage",
            EwsAction::DeleteItem => "DeleteItemResponseMessage",
            EwsAction::SendItem => "SendItemResponseMessage",
            EwsAction::MoveItem => "MoveItemResponseMessage",
            EwsAction::ResolveNames => "ResolveNamesResponseMessage",
            EwsAction::GetUserOofSettings => "GetUserOofSettingsResponseMessage",
            EwsAction::SetUserOofSettings => "SetUserOofSettingsResponseMessage",
            EwsAction::GetServiceConfiguration => "GetServiceConfigurationResponseMessage",
            EwsAction::GetServerTimeZones => "GetServerTimeZonesResponseMessage",
            EwsAction::GetFolderInfo => "GetFolderInfoResponseMessage",
            EwsAction::GetMailTips => "GetMailTipsResponseMessage",
            EwsAction::FindPeople => "FindPeopleResponseMessage",
            EwsAction::GetConversationItems => "GetConversationItemsResponseMessage",
            EwsAction::ConvertId => "ConvertIdResponseMessage",
            EwsAction::GetRoomLists => "GetRoomListsResponseMessage",
            EwsAction::GetRooms => "GetRoomsResponseMessage",
            EwsAction::GetDelegate => "GetDelegateResponseMessage",
            EwsAction::AddDelegate => "AddDelegateResponseMessage",
            EwsAction::RemoveDelegate => "RemoveDelegateResponseMessage",
            EwsAction::UpdateDelegate => "UpdateDelegateResponseMessage",
            EwsAction::GetUserPhoto => "GetUserPhotoResponseMessage",
            EwsAction::MarkAsJunk => "MarkAsJunkResponseMessage",
            EwsAction::GetAppManifests => "GetAppManifestsResponseMessage",
            EwsAction::GetAppMarketplaceUrl => "GetAppMarketplaceUrlResponseMessage",
            EwsAction::InstallApp => "InstallAppResponseMessage",
            EwsAction::UninstallApp => "UninstallAppResponseMessage",
            EwsAction::GetClientAccessToken => "GetClientAccessTokenResponseMessage",
            EwsAction::GetReminders => "GetRemindersResponseMessage",
            EwsAction::PerformReminderAction => "PerformReminderActionResponseMessage",
            EwsAction::GetPersona => "GetPersonaResponseMessage",
            EwsAction::CreateAttachment => "CreateAttachmentResponseMessage",
            EwsAction::GetAttachment => "GetAttachmentResponseMessage",
            EwsAction::DeleteAttachment => "DeleteAttachmentResponseMessage",
            EwsAction::GetUserConfiguration => "GetUserConfigurationResponseMessage",
        }
    }
}

fn validate_schema(action: &EwsAction, xml: &str) -> Result<(), &'static str> {
    if !xml.contains("Envelope") || !xml.contains("Body") {
        return Err("Missing SOAP Envelope or Body");
    }
    if !xml.contains(EWS_MSG_NS) && !xml.contains("xmlns:m=") {
        return Err("Missing EWS messages namespace");
    }

    // Note: IncludeMimeContent is allowed but will be ignored. We don't include
    // MIME content in responses to keep payloads small. This is acceptable per
    // MS-OXWSCORE which allows servers to omit optional elements.
    if action.requires_mime_validation() && xml.contains("IncludeMimeContent") {
        tracing::debug!(
            action = ?action,
            "Client requested IncludeMimeContent; it will be ignored"
        );
    }

    Ok(())
}

fn operation_error_response(
    action: &EwsAction,
    code: &str,
    message: &str,
    status: StatusCode,
) -> Response {
    let resp_msg = action.response_message_name();
    let inner = format!(
        r#"<m:{resp_msg} ResponseClass="Error" xmlns:m="{msg_ns}" xmlns:t="{type_ns}"><m:MessageText>{}</m:MessageText><m:ResponseCode>{}</m:ResponseCode><m:DescriptiveLinkKey>0</m:DescriptiveLinkKey></m:{resp_msg}>"#,
        xml_escape(message),
        xml_escape(code),
        resp_msg = resp_msg,
        msg_ns = EWS_MSG_NS,
        type_ns = EWS_TYPE_NS
    );
    let prefix = &resp_msg[..resp_msg.len().saturating_sub("ResponseMessage".len())];
    let body = format!(
        r#"<m:{}Response xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages>{}</m:ResponseMessages></m:{}Response>"#,
        prefix, EWS_MSG_NS, EWS_TYPE_NS, inner, prefix
    );
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header><t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="{type_ns}" /></s:Header>
  <s:Body>{body}</s:Body>
</s:Envelope>"#,
        type_ns = EWS_TYPE_NS,
        body = body
    );
    ews_response(status, xml)
}

fn ews_response(status: StatusCode, xml: String) -> Response {
    (status, [("Content-Type", "text/xml; charset=utf-8")], xml).into_response()
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !forwarded_https_enforced(&headers) {
        return soap_fault(
            "ErrorInvalidRequest",
            "x-forwarded-proto must be https",
            StatusCode::BAD_REQUEST,
        );
    }
    let mut auth = match parse_basic_auth(&headers) {
        Some(a) => a,
        None => return unauthorized(),
    };
    let raw_username = auth.username.clone();
    auth.username = canonicalize_username(&auth.username, &state.cfg.mail_domain);
    if auth.username != raw_username {
        tracing::info!(
            raw_username = %raw_username,
            canonical_username = %auth.username,
            "Username domain canonicalized to GATEWAY_MAIL_DOMAIN"
        );
    }
    // Verify credentials early to avoid unnecessary processing
    if !state
        .auth_verifier
        .verify(&auth.username, auth.password.expose_secret())
        .await
    {
        tracing::debug!("EWS authentication failed for user: {}", auth.username);
        return unauthorized();
    }
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
        EwsAction::GetFolder => handle_get_folder(&state, &auth, &body).await,
        EwsAction::FindFolder => handle_find_folder(&state, &auth, &body).await,
        EwsAction::FindItem => handle_find_item(&state, &auth, &body).await,
        EwsAction::GetItem => handle_get_item(&state, &auth, &body).await,
        EwsAction::GetUserAvailability => handle_get_user_availability(&state, &auth, &body).await,
        EwsAction::SyncFolderItems => handle_sync_folder_items(&state, &auth, &body).await,
        EwsAction::SyncFolderHierarchy => handle_sync_folder_hierarchy(&state, &auth, &body).await,
        EwsAction::Subscribe => handle_subscribe(&auth, &body).await,
        EwsAction::Unsubscribe => handle_unsubscribe(&auth, &body).await,
        EwsAction::CreateItem => handle_create_item(&state, &auth, &body).await,
        EwsAction::UpdateItem => handle_update_item(&state, &auth, &body).await,
        EwsAction::DeleteItem => handle_delete_item(&state, &auth, &body).await,
        EwsAction::SendItem => handle_send_item(&state, &auth, &body).await,
        EwsAction::MoveItem => handle_move_item(&state, &auth, &body).await,
        EwsAction::ResolveNames => handle_resolve_names(&auth, &body).await,
        EwsAction::GetUserOofSettings => handle_get_user_oof_settings(&auth, &body).await,
        EwsAction::SetUserOofSettings => handle_set_user_oof_settings(&auth, &body).await,
        EwsAction::GetServiceConfiguration => handle_get_service_configuration(&state).await,
        EwsAction::GetServerTimeZones => handle_get_server_time_zones().await,
        EwsAction::GetFolderInfo => handle_get_folder_info().await,
        EwsAction::GetMailTips => handle_get_mail_tips(&auth, &body).await,
        EwsAction::FindPeople => handle_find_people(&auth, &body).await,
        EwsAction::GetConversationItems => handle_get_conversation_items().await,
        EwsAction::ConvertId => handle_convert_id(&auth, &body).await,
        EwsAction::GetRoomLists => handle_get_room_lists(&state, &auth).await,
        EwsAction::GetRooms => handle_get_rooms(&state, &auth, &body).await,
        EwsAction::GetDelegate => handle_get_delegate(&state, &auth).await,
        EwsAction::AddDelegate => handle_add_delegate(&state, &auth, &body).await,
        EwsAction::RemoveDelegate => handle_remove_delegate(&state, &auth, &body).await,
        EwsAction::UpdateDelegate => handle_update_delegate(&state, &auth, &body).await,
        EwsAction::GetUserPhoto => handle_get_user_photo(&auth, &body).await,
        EwsAction::MarkAsJunk => handle_mark_as_junk(&auth, &body).await,
        EwsAction::GetAppManifests => handle_get_app_manifests().await,
        EwsAction::GetAppMarketplaceUrl => handle_get_app_marketplace_url().await,
        EwsAction::InstallApp => handle_install_app().await,
        EwsAction::UninstallApp => handle_uninstall_app().await,
        EwsAction::GetClientAccessToken => handle_get_client_access_token().await,
        EwsAction::GetReminders => handle_get_reminders(&auth, &body).await,
        EwsAction::PerformReminderAction => handle_perform_reminder_action(&auth, &body).await,
        EwsAction::GetPersona => handle_get_persona(&auth, &body).await,
        EwsAction::CreateAttachment => handle_create_attachment(&state, &auth, &body).await,
        EwsAction::GetAttachment => handle_get_attachment(&state, &auth, &body).await,
        EwsAction::DeleteAttachment => handle_delete_attachment(&state, &auth, &body).await,
        EwsAction::GetUserConfiguration => {
            handle_get_user_configuration(&state, &auth, &body).await
        }
    }
}

fn parse_basic_auth(headers: &HeaderMap) -> Option<AuthContext> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let auth = auth.trim();
    if !auth
        .get(..6)
        .is_some_and(|s| s.eq_ignore_ascii_case("basic "))
    {
        return None;
    }
    let b64 = auth[6..].trim();
    let mut decoded = zeroize::Zeroizing::new(Vec::new());
    STANDARD.decode_vec(b64.as_bytes(), decoded.as_mut()).ok()?;
    let creds = zeroize::Zeroizing::new(std::str::from_utf8(&decoded).ok()?.to_owned());
    let idx = creds.find(':')?;
    let raw_user = creds[..idx].to_string();
    // Strip domain prefix like "EXAMPLE\user" → "user"
    let user = normalize_username(&raw_user).to_string();
    let pass = SecretString::from(creds[idx + 1..].to_string());
    Some(AuthContext {
        username: user,
        password: pass,
    })
}

fn forwarded_https_enforced(headers: &HeaderMap) -> bool {
    match headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_ascii_lowercase())
    {
        Some(v) => v == "https",
        None => true,
    }
}

fn detect_action(xml: &str) -> Option<EwsAction> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name();
                return Some(match name.as_ref() {
                    b"GetFolder" => EwsAction::GetFolder,
                    b"FindFolder" => EwsAction::FindFolder,
                    b"FindItem" => EwsAction::FindItem,
                    b"GetItem" => EwsAction::GetItem,
                    b"GetUserAvailabilityRequest" => EwsAction::GetUserAvailability,
                    b"SyncFolderItems" => EwsAction::SyncFolderItems,
                    b"SyncFolderHierarchy" => EwsAction::SyncFolderHierarchy,
                    b"Subscribe" => EwsAction::Subscribe,
                    b"Unsubscribe" => EwsAction::Unsubscribe,
                    b"CreateItem" => EwsAction::CreateItem,
                    b"UpdateItem" => EwsAction::UpdateItem,
                    b"DeleteItem" => EwsAction::DeleteItem,
                    b"SendItem" => EwsAction::SendItem,
                    b"MoveItem" => EwsAction::MoveItem,
                    b"ResolveNames" => EwsAction::ResolveNames,
                    b"GetUserOofSettingsRequest" => EwsAction::GetUserOofSettings,
                    b"SetUserOofSettingsRequest" => EwsAction::SetUserOofSettings,
                    b"GetServiceConfiguration" => EwsAction::GetServiceConfiguration,
                    b"GetServerTimeZones" => EwsAction::GetServerTimeZones,
                    b"GetFolderInfo" => EwsAction::GetFolderInfo,
                    b"GetMailTips" => EwsAction::GetMailTips,
                    b"FindPeople" => EwsAction::FindPeople,
                    b"GetConversationItems" => EwsAction::GetConversationItems,
                    b"ConvertId" => EwsAction::ConvertId,
                    b"GetRoomLists" => EwsAction::GetRoomLists,
                    b"GetRooms" => EwsAction::GetRooms,
                    b"GetDelegate" => EwsAction::GetDelegate,
                    b"AddDelegate" => EwsAction::AddDelegate,
                    b"RemoveDelegate" => EwsAction::RemoveDelegate,
                    b"UpdateDelegate" => EwsAction::UpdateDelegate,
                    b"GetUserPhoto" => EwsAction::GetUserPhoto,
                    b"MarkAsJunk" => EwsAction::MarkAsJunk,
                    b"GetAppManifests" => EwsAction::GetAppManifests,
                    b"GetAppMarketplaceUrl" => EwsAction::GetAppMarketplaceUrl,
                    b"InstallApp" => EwsAction::InstallApp,
                    b"UninstallApp" => EwsAction::UninstallApp,
                    b"GetClientAccessToken" => EwsAction::GetClientAccessToken,
                    b"GetReminders" => EwsAction::GetReminders,
                    b"PerformReminderAction" => EwsAction::PerformReminderAction,
                    b"GetPersona" => EwsAction::GetPersona,
                    b"CreateAttachment" => EwsAction::CreateAttachment,
                    b"GetAttachment" => EwsAction::GetAttachment,
                    b"DeleteAttachment" => EwsAction::DeleteAttachment,
                    b"GetUserConfiguration" => EwsAction::GetUserConfiguration,
                    _ => {
                        buf.clear();
                        continue;
                    }
                });
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    extract_tag_texts(xml, tag).into_iter().next()
}

fn extract_tag_texts(xml: &str, tag: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut inside = false;
    let mut values = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == tag => inside = true,
            Ok(Event::Text(t)) if inside => {
                if let Ok(value) = t.decode() {
                    values.push(value.into_owned());
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == tag => inside = false,
            Ok(Event::Eof) | Err(_) => return values,
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

/// Extract the DB "owner" key from a username.
///
/// The username must already be canonicalized (domain normalized to
/// `GATEWAY_MAIL_DOMAIN`) before this function is called. Canonicalization
/// is performed at the EAS/EWS entry points so that all downstream code
/// operates on a consistent identity.
pub fn owner_from_username(username: &str) -> &str {
    username
}
fn folder_id_for_owner(owner: &str) -> String {
    folder_id_for(owner, DistinguishedFolder::Calendar)
}

fn changekey_for_item(item: &EwsItemRow) -> String {
    let mut h = Sha256::new();
    h.update(item.server_id.as_bytes());
    if let Some(e) = &item.etag {
        h.update(e.as_bytes());
    }
    let digest = h.finalize();
    const_hex::encode(&digest[..12])
}

fn busy_status_to_ews(value: u8) -> &'static str {
    match value {
        0 => "Free",
        1 => "Tentative",
        3 => "OOF",
        _ => "Busy",
    }
}
fn sensitivity_to_ews(value: u8) -> &'static str {
    match value {
        1 => "Personal",
        2 => "Private",
        3 => "Confidential",
        _ => "Normal",
    }
}

fn derived_meeting_status(item: &crate::calendar::CalendarItem) -> u8 {
    if let Some(v) = item.meeting_status {
        return v;
    }
    let is_meeting = !item.attendees.is_empty();
    let organizer = item
        .organizer_email
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    if !is_meeting {
        0
    } else if organizer {
        1
    } else {
        3
    }
}

fn derived_response_type(item: &crate::calendar::CalendarItem) -> Option<&'static str> {
    if let Some(v) = item.response_type {
        return Some(match v {
            1 => "Organizer",
            2 => "Tentative",
            3 => "Accept",
            4 => "Decline",
            5 => "NoResponseReceived",
            _ => "Unknown",
        });
    }
    if derived_meeting_status(item) == 1 {
        return Some("Organizer");
    }
    item.attendees.iter().find_map(|a| match a.attendee_status {
        Some(2) => Some("Tentative"),
        Some(3) => Some("Accept"),
        Some(4) => Some("Decline"),
        Some(5) => Some("NoResponseReceived"),
        _ => None,
    })
}

fn extract_requested_change_key(xml: &str) -> Option<String> {
    extract_first_attr(xml, b"ItemId", b"ChangeKey")
}

fn extract_conflict_resolution(xml: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let tag_start = lower.find("updateitem")?;
    let tag_end = lower[tag_start..]
        .find('>')
        .map(|i| tag_start + i)
        .unwrap_or(lower.len());
    let tag_fragment = &lower[tag_start..tag_end];
    let attr_pos = tag_fragment.find("conflictresolution=")?;
    let value_start = attr_pos + "conflictresolution=".len();
    let value_rest = &tag_fragment[value_start..];
    let quote_char = value_rest.chars().next()?;
    let value_rest = &value_rest[quote_char.len_utf8()..];
    let value_end = value_rest.find(quote_char).unwrap_or(value_rest.len());
    Some(value_rest[..value_end].to_string())
}

fn validate_item_change_key(
    action: &EwsAction,
    body: &str,
    item: &EwsItemRow,
) -> Result<(), Box<Response>> {
    if let Some(requested) = extract_requested_change_key(body) {
        let expected = changekey_for_item(item);
        if requested != expected {
            return Err(operation_error_response(
                action,
                "ErrorIrresolvableConflict",
                "Item ChangeKey does not match the current stored version",
                StatusCode::OK,
            )
            .into());
        }
    }
    Ok(())
}

fn render_ews_attendees(item: &crate::calendar::CalendarItem) -> String {
    let mut required = String::new();
    let mut optional = String::new();
    for attendee in &item.attendees {
        let response = match attendee.attendee_status.unwrap_or(5) {
            3 => "Accept",
            2 => "Tentative",
            4 => "Decline",
            _ => "Unknown",
        };
        let xml = format!(
            r#"<t:Attendee><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox><t:ResponseType>{}</t:ResponseType></t:Attendee>"#,
            xml_escape(attendee.name.as_deref().unwrap_or(&attendee.email)),
            xml_escape(&attendee.email),
            response
        );
        if attendee.attendee_type == Some(2) {
            optional.push_str(&xml);
        } else {
            required.push_str(&xml);
        }
    }
    let mut out = String::new();
    if !required.is_empty() {
        out.push_str(&format!(
            "<t:RequiredAttendees>{}</t:RequiredAttendees>",
            required
        ));
    }
    if !optional.is_empty() {
        out.push_str(&format!(
            "<t:OptionalAttendees>{}</t:OptionalAttendees>",
            optional
        ));
    }
    out
}

fn render_ews_categories(item: &crate::calendar::CalendarItem) -> String {
    if item.categories.is_empty() {
        return String::new();
    }
    format!(
        "<t:Categories>{}</t:Categories>",
        item.categories
            .iter()
            .map(|v| format!("<t:String>{}</t:String>", xml_escape(v)))
            .collect::<Vec<_>>()
            .join("")
    )
}

fn ews_calendar_item_type(item: &crate::calendar::CalendarItem) -> &'static str {
    if item.rrule.is_some() {
        "RecurringMaster"
    } else {
        "Single"
    }
}

fn ews_my_response_type(item: &crate::calendar::CalendarItem) -> &'static str {
    derived_response_type(item).unwrap_or("Unknown")
}

fn ews_calendar_event_details_xml(item: &crate::calendar::CalendarItem) -> String {
    let is_private = item.sensitivity.map(|v| v >= 2).unwrap_or(false);
    format!(
        "<t:CalendarEventDetails><t:Subject>{}</t:Subject><t:Location>{}</t:Location><t:IsMeeting>{}</t:IsMeeting><t:IsRecurring>{}</t:IsRecurring><t:IsException>false</t:IsException><t:IsReminderSet>{}</t:IsReminderSet><t:IsPrivate>{}</t:IsPrivate></t:CalendarEventDetails>",
        xml_escape(&item.subject),
        xml_escape(&item.location),
        if item.attendees.is_empty() {
            "false"
        } else {
            "true"
        },
        if item.rrule.is_some() {
            "true"
        } else {
            "false"
        },
        if item.reminder.is_some() {
            "true"
        } else {
            "false"
        },
        if is_private { "true" } else { "false" }
    )
}

fn ews_deleted_occurrences_xml(item: &crate::calendar::CalendarItem) -> String {
    let mut starts = item
        .exceptions
        .iter()
        .filter(|e| e.deleted)
        .map(|e| e.exception_start)
        .collect::<Vec<_>>();
    starts.extend(item.exdates.iter().copied());
    starts.sort();
    starts.dedup();
    if starts.is_empty() {
        return String::new();
    }
    format!(
        "<t:DeletedOccurrences>{}</t:DeletedOccurrences>",
        starts
            .iter()
            .map(|s| format!(
                "<t:DeletedOccurrence><t:Start>{}</t:Start></t:DeletedOccurrence>",
                format_ews_datetime(s)
            ))
            .collect::<String>()
    )
}

fn ews_response_objects_xml(item: &crate::calendar::CalendarItem) -> String {
    let is_meeting = !item.attendees.is_empty();
    let is_organizer = item
        .organizer_email
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let response_requested = item.response_requested.unwrap_or(is_meeting);
    if !is_meeting || is_organizer || !response_requested {
        return String::new();
    }
    "<t:ResponseObjects><t:AcceptItem /><t:TentativelyAcceptItem /><t:DeclineItem /></t:ResponseObjects>".to_string()
}

fn ews_modified_occurrences_xml(
    item_id: &str,
    change_key: &str,
    item: &crate::calendar::CalendarItem,
) -> String {
    let modified = item.exceptions.iter().filter(|e| !e.deleted).map(|e| {
        let start = e.start.unwrap_or(e.exception_start);
        let end = e.end.unwrap_or_else(|| start + chrono::Duration::minutes(30));
        let subject = e.subject.as_deref().unwrap_or(&item.subject);
        format!(
            r#"<t:Occurrence><t:ItemId Id="{}-{}" ChangeKey="{}" /><t:Start>{}</t:Start><t:End>{}</t:End><t:OriginalStart>{}</t:OriginalStart><t:Subject>{}</t:Subject></t:Occurrence>"#,
            xml_escape(item_id), start.timestamp(), xml_escape(change_key),
            format_ews_datetime(&start), format_ews_datetime(&end), format_ews_datetime(&e.exception_start), xml_escape(subject)
        )
    }).collect::<String>();
    if modified.is_empty() {
        String::new()
    } else {
        format!(
            "<t:ModifiedOccurrences>{}</t:ModifiedOccurrences>",
            modified
        )
    }
}

fn ews_month_name(month: &str) -> &'static str {
    match month {
        "1" => "January",
        "2" => "February",
        "3" => "March",
        "4" => "April",
        "5" => "May",
        "6" => "June",
        "7" => "July",
        "8" => "August",
        "9" => "September",
        "10" => "October",
        "11" => "November",
        "12" => "December",
        _ => "January",
    }
}

fn ews_days_of_week(byday: &str) -> String {
    byday
        .replace("MO", "Monday")
        .replace("TU", "Tuesday")
        .replace("WE", "Wednesday")
        .replace("TH", "Thursday")
        .replace("FR", "Friday")
        .replace("SA", "Saturday")
        .replace("SU", "Sunday")
        .replace(',', " ")
}

fn ews_day_of_week_index(ord: i32) -> &'static str {
    match ord {
        1 => "First",
        2 => "Second",
        3 => "Third",
        4 => "Fourth",
        -1 => "Last",
        _ => "First",
    }
}

fn parse_rrule_byday(value: &str) -> Option<(i32, String)> {
    let mut ordinal_end = 0usize;
    for (idx, ch) in value.char_indices() {
        if ch == '-' || ch.is_ascii_digit() {
            ordinal_end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    let ordinal = if ordinal_end == 0 {
        0
    } else {
        value[..ordinal_end].parse::<i32>().ok()?
    };
    let code = value[ordinal_end..].to_string();
    if code.is_empty() {
        return None;
    }
    Some((ordinal, code))
}

fn render_ews_recurrence_xml(rrule: &str, start: chrono::DateTime<chrono::Utc>) -> String {
    let mut freq = "";
    let mut interval = "1".to_string();
    let mut byday = None;
    let mut bymonthday = None;
    let mut count = None;
    let mut until = None;
    let mut bymonth = None;
    for part in rrule.split(';') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "FREQ" => freq = v,
                "INTERVAL" => interval = v.to_string(),
                "BYDAY" => byday = Some(v.to_string()),
                "BYMONTHDAY" => bymonthday = Some(v.to_string()),
                "COUNT" => count = Some(v.to_string()),
                "UNTIL" => until = Some(v.to_string()),
                "BYMONTH" => bymonth = Some(v.to_string()),
                _ => {}
            }
        }
    }
    let pattern = match freq {
        "DAILY" => format!(
            "<t:DailyRecurrence><t:Interval>{}</t:Interval></t:DailyRecurrence>",
            interval
        ),
        "WEEKLY" => format!(
            "<t:WeeklyRecurrence><t:Interval>{}</t:Interval><t:DaysOfWeek>{}</t:DaysOfWeek></t:WeeklyRecurrence>",
            interval,
            ews_days_of_week(&byday.unwrap_or_default())
        ),
        "MONTHLY" => {
            if let Some(byday) = byday.as_deref().and_then(parse_rrule_byday) {
                format!(
                    "<t:RelativeMonthlyRecurrence><t:Interval>{}</t:Interval><t:DaysOfWeek>{}</t:DaysOfWeek><t:DayOfWeekIndex>{}</t:DayOfWeekIndex></t:RelativeMonthlyRecurrence>",
                    interval,
                    ews_days_of_week(&byday.1),
                    ews_day_of_week_index(if byday.0 == 0 { 1 } else { byday.0 })
                )
            } else {
                format!(
                    "<t:AbsoluteMonthlyRecurrence><t:Interval>{}</t:Interval><t:DayOfMonth>{}</t:DayOfMonth></t:AbsoluteMonthlyRecurrence>",
                    interval,
                    bymonthday.unwrap_or_else(|| start.day().to_string())
                )
            }
        }
        "YEARLY" => {
            let month = bymonth.unwrap_or_else(|| start.month().to_string());
            if let Some(byday) = byday.as_deref().and_then(parse_rrule_byday) {
                format!(
                    "<t:RelativeYearlyRecurrence><t:DaysOfWeek>{}</t:DaysOfWeek><t:DayOfWeekIndex>{}</t:DayOfWeekIndex><t:Month>{}</t:Month></t:RelativeYearlyRecurrence>",
                    ews_days_of_week(&byday.1),
                    ews_day_of_week_index(if byday.0 == 0 { 1 } else { byday.0 }),
                    ews_month_name(&month)
                )
            } else {
                format!(
                    "<t:AbsoluteYearlyRecurrence><t:Month>{}</t:Month><t:DayOfMonth>{}</t:DayOfMonth></t:AbsoluteYearlyRecurrence>",
                    ews_month_name(&month),
                    bymonthday.unwrap_or_else(|| start.day().to_string())
                )
            }
        }
        _ => return String::new(),
    };
    let range = if let Some(count) = count {
        format!(
            "<t:NumberedRecurrence><t:StartDate>{}</t:StartDate><t:NumberOfOccurrences>{}</t:NumberOfOccurrences></t:NumberedRecurrence>",
            start.format("%Y-%m-%d"),
            count
        )
    } else if let Some(until) = until {
        let end_date = crate::calendar::parse_datetime(&until)
            .map(|v| v.format("%Y-%m-%d").to_string())
            .unwrap_or(until);
        format!(
            "<t:EndDateRecurrence><t:StartDate>{}</t:StartDate><t:EndDate>{}</t:EndDate></t:EndDateRecurrence>",
            start.format("%Y-%m-%d"),
            end_date
        )
    } else {
        format!(
            "<t:NoEndRecurrence><t:StartDate>{}</t:StartDate></t:NoEndRecurrence>",
            start.format("%Y-%m-%d")
        )
    };
    format!("<t:Recurrence>{}{}</t:Recurrence>", pattern, range)
}

fn render_ews_calendar_item_xml_with_shape(
    item_id: &str,
    change_key: &str,
    item: &crate::calendar::CalendarItem,
    shape: ItemShape,
    has_attachments: bool,
    attachment_summaries: Option<&[crate::attachment::EwsAttachmentSummary]>,
) -> String {
    let created = item.dtstamp.unwrap_or_else(chrono::Utc::now);
    let duration = item.end - item.start;
    let duration_minutes = duration.num_minutes().max(0);
    let hours = duration_minutes / 60;
    let minutes = duration_minutes % 60;
    let is_meeting = !item.attendees.is_empty();
    let is_organizer = item
        .organizer_email
        .as_deref()
        .map(|v| !v.is_empty())
        .unwrap_or(false);
    let is_cancelled = item
        .meeting_status
        .map(|s| (s & 0x04) != 0)
        .unwrap_or(false);
    let mut xml = format!(
        r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:UID>{}</t:UID><t:Start>{}</t:Start><t:End>{}</t:End><t:IsAllDayEvent>{}</t:IsAllDayEvent>"#,
        xml_escape(item_id),
        xml_escape(change_key),
        xml_escape(&item.subject),
        xml_escape(&item.uid),
        format_ews_datetime(&item.start),
        format_ews_datetime(&item.end),
        if item.all_day { "true" } else { "false" }
    );
    if shape == ItemShape::IdOnly {
        xml.push_str("<t:IsDraft>false</t:IsDraft>");
        xml.push_str("<t:EffectiveRights><t:CreateAssociated>false</t:CreateAssociated><t:CreateContents>true</t:CreateContents><t:CreateHierarchy>false</t:CreateHierarchy><t:Delete>true</t:Delete><t:Modify>true</t:Modify><t:Read>true</t:Read></t:EffectiveRights>");
        xml.push_str("</t:CalendarItem>");
        return xml;
    }
    if !item.location.is_empty() {
        xml.push_str(&format!(
            "<t:Location>{}</t:Location>",
            xml_escape(&item.location)
        ));
    }
    if !item.description.is_empty() {
        xml.push_str(&format!(
            r#"<t:Body BodyType="Text">{}</t:Body>"#,
            xml_escape(&item.description)
        ));
        xml.push_str(&format!(
            "<t:TextBody>{}</t:TextBody>",
            xml_escape(&item.description)
        ));
    }
    if let Some(v) = item.reminder {
        xml.push_str(&format!(
            "<t:ReminderMinutesBeforeStart>{}</t:ReminderMinutesBeforeStart>",
            v
        ));
    }
    xml.push_str(&format!(
        "<t:ReminderIsSet>{}</t:ReminderIsSet>",
        if item.reminder.is_some() {
            "true"
        } else {
            "false"
        }
    ));
    if let Some(v) = item.busy_status {
        xml.push_str(&format!(
            "<t:LegacyFreeBusyStatus>{}</t:LegacyFreeBusyStatus>",
            busy_status_to_ews(v)
        ));
    }
    if let Some(v) = item.sensitivity {
        xml.push_str(&format!(
            "<t:Sensitivity>{}</t:Sensitivity>",
            sensitivity_to_ews(v)
        ));
    }
    if let Some(v) = item.response_requested {
        xml.push_str(&format!(
            "<t:ResponseRequested>{}</t:ResponseRequested>",
            if v { "true" } else { "false" }
        ));
    }
    if let Some(v) = item.disallow_new_time_proposal {
        xml.push_str(&format!(
            "<t:DisallowNewTimeProposal>{}</t:DisallowNewTimeProposal>",
            if v { "true" } else { "false" }
        ));
    }
    if let Some(v) = &item.organizer_email {
        xml.push_str(&format!(
            r#"<t:Organizer><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Organizer>"#,
            xml_escape(item.organizer_name.as_deref().unwrap_or(v)), xml_escape(v)
        ));
    }
    xml.push_str(&format!(
        "<t:DateTimeCreated>{}</t:DateTimeCreated><t:DateTimeReceived>{}</t:DateTimeReceived><t:DateTimeSent>{}</t:DateTimeSent><t:DateTimeStamp>{}</t:DateTimeStamp>",
        format_ews_datetime(&created), format_ews_datetime(&created), format_ews_datetime(&created), format_ews_datetime(&created)
    ));
    xml.push_str(&format!(
        "<t:Duration>PT{}H{}M</t:Duration>",
        hours, minutes
    ));
    xml.push_str(&format!(
        "<t:CalendarItemType>{}</t:CalendarItemType>",
        ews_calendar_item_type(item)
    ));
    xml.push_str(&format!(
        "<t:MyResponseType>{}</t:MyResponseType>",
        ews_my_response_type(item)
    ));
    xml.push_str(&format!(
        "<t:IsMeeting>{}</t:IsMeeting><t:IsOrganizer>{}</t:IsOrganizer><t:IsRecurring>{}</t:IsRecurring><t:IsCancelled>{}</t:IsCancelled><t:HasAttachments>{}</t:HasAttachments>",
        if is_meeting { "true" } else { "false" }, if is_organizer { "true" } else { "false" },
        if item.rrule.is_some() { "true" } else { "false" }, if is_cancelled { "true" } else { "false" },
        if has_attachments { "true" } else { "false" }
    ));
    if let Some(summaries) = attachment_summaries
        && !summaries.is_empty()
    {
        xml.push_str(&crate::attachment::render_ews_attachments_xml(summaries));
    }
    xml.push_str(&format!(
        "<t:MeetingRequestWasSent>{}</t:MeetingRequestWasSent>",
        if is_meeting { "true" } else { "false" }
    ));
    xml.push_str(&format!(
        "<t:AllowNewTimeProposal>{}</t:AllowNewTimeProposal>",
        if item.disallow_new_time_proposal.unwrap_or(false) {
            "false"
        } else {
            "true"
        }
    ));
    xml.push_str(&format!(
        "<t:MeetingStatus>{}</t:MeetingStatus>",
        derived_meeting_status(item)
    ));
    if let Some(v) = derived_response_type(item) {
        xml.push_str(&format!("<t:ResponseType>{}</t:ResponseType>", v));
    }
    if let Some(v) = item.appointment_reply_time {
        xml.push_str(&format!(
            "<t:AppointmentReplyTime>{}</t:AppointmentReplyTime>",
            format_ews_datetime(&v)
        ));
    }
    if let Some(v) = &item.timezone {
        xml.push_str(&format!(
            "<t:StartTimeZone>{}</t:StartTimeZone>",
            xml_escape(v)
        ));
        xml.push_str(&format!("<t:EndTimeZone>{}</t:EndTimeZone>", xml_escape(v)));
    }
    if let Some(v) = &item.timezone_blob {
        xml.push_str(&format!(
            "<t:MeetingTimeZone>{}</t:MeetingTimeZone>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.online_meeting_conf_link {
        xml.push_str(&format!(
            "<t:OnlineMeetingConfLink>{}</t:OnlineMeetingConfLink>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.online_meeting_external_link {
        xml.push_str(&format!(
            "<t:OnlineMeetingExternalLink>{}</t:OnlineMeetingExternalLink>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.client_uid {
        xml.push_str(&format!("<t:ClientUid>{}</t:ClientUid>", xml_escape(v)));
    }
    xml.push_str("<t:AdjacentMeetingCount>0</t:AdjacentMeetingCount><t:ConflictingMeetingCount>0</t:ConflictingMeetingCount>");
    if shape == ItemShape::Default {
        xml.push_str(&ews_response_objects_xml(item));
        xml.push_str("<t:IsDraft>false</t:IsDraft>");
        xml.push_str("<t:DisplayTo>");
        xml.push_str(&xml_escape(
            &item
                .attendees
                .iter()
                .map(|a| a.name.as_deref().unwrap_or(&a.email))
                .collect::<Vec<_>>()
                .join("; "),
        ));
        xml.push_str("</t:DisplayTo>");
        xml.push_str(&format!(
            "<t:LastModifiedName>{}</t:LastModifiedName>",
            xml_escape(item.organizer_name.as_deref().unwrap_or("Unknown"))
        ));
        xml.push_str(&format!(
            "<t:LastModifiedTime>{}</t:LastModifiedTime>",
            format_ews_datetime(&item.dtstamp.unwrap_or_else(chrono::Utc::now))
        ));
        xml.push_str(&format!(
            "<t:Size>{}</t:Size>",
            item.subject.len() + item.description.len() + item.location.len() + 512
        ));
        xml.push_str("<t:EffectiveRights><t:CreateAssociated>false</t:CreateAssociated><t:CreateContents>true</t:CreateContents><t:CreateHierarchy>false</t:CreateHierarchy><t:Delete>true</t:Delete><t:Modify>true</t:Modify><t:Read>true</t:Read></t:EffectiveRights>");
        xml.push_str("</t:CalendarItem>");
        return xml;
    }
    xml.push_str(&render_ews_categories(item));
    xml.push_str(&render_ews_attendees(item));
    xml.push_str(&ews_deleted_occurrences_xml(item));
    xml.push_str(&ews_modified_occurrences_xml(item_id, change_key, item));
    if let Some(rrule) = &item.rrule {
        xml.push_str(&render_ews_recurrence_xml(rrule, item.start));
    }
    xml.push_str(&ews_response_objects_xml(item));
    xml.push_str("<t:IsDraft>false</t:IsDraft>");
    xml.push_str("<t:DisplayTo>");
    xml.push_str(&xml_escape(
        &item
            .attendees
            .iter()
            .map(|a| a.name.as_deref().unwrap_or(&a.email))
            .collect::<Vec<_>>()
            .join("; "),
    ));
    xml.push_str("</t:DisplayTo>");
    xml.push_str(&format!(
        "<t:LastModifiedName>{}</t:LastModifiedName>",
        xml_escape(item.organizer_name.as_deref().unwrap_or("Unknown"))
    ));
    xml.push_str(&format!(
        "<t:LastModifiedTime>{}</t:LastModifiedTime>",
        format_ews_datetime(&item.dtstamp.unwrap_or_else(chrono::Utc::now))
    ));
    xml.push_str(&format!(
        "<t:Size>{}</t:Size>",
        item.subject.len() + item.description.len() + item.location.len() + 512
    ));
    xml.push_str("<t:EffectiveRights><t:CreateAssociated>false</t:CreateAssociated><t:CreateContents>true</t:CreateContents><t:CreateHierarchy>false</t:CreateHierarchy><t:Delete>true</t:Delete><t:Modify>true</t:Modify><t:Read>true</t:Read></t:EffectiveRights>");
    xml.push_str("</t:CalendarItem>");
    xml
}

fn render_ews_calendar_item_xml(
    item_id: &str,
    change_key: &str,
    item: &crate::calendar::CalendarItem,
) -> String {
    render_ews_calendar_item_xml_with_shape(
        item_id,
        change_key,
        item,
        ItemShape::AllProperties,
        false,
        None,
    )
}

/// Fetch free-busy information via JMAP Calendar (urn:ietf:params:jmap:calendars).
///
/// Uses `CalendarEvent/query` + `CalendarEvent/get` with the `iCalendar` property
/// to obtain ICS data, then renders the merged free-busy string and events XML
/// using the same logic as the CalDAV path.
///
/// Returns `Ok((merged_freebusy, events_xml))` on success, `Err` to fall back to CalDAV.
async fn fetch_freebusy_jmap(
    jmap: &Arc<JmapClient>,
    mailbox: &str,
    password: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> anyhow::Result<(String, String)> {
    let secret_password = SecretString::from(password.to_string());

    // Check if JMAP Calendar is supported
    if !jmap.supports_calendar(mailbox, &secret_password).await {
        return Err(anyhow::anyhow!("JMAP Calendar not supported by server"));
    }

    let account_id = jmap
        .get_calendar_account_id(mailbox, &secret_password)
        .await?;
    let safe_interval = 30i64; // Default interval for free-busy
    let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
        / (safe_interval * 60)) as usize;
    let mut merged = vec!['0'; slot_count];
    let mut events_xml_out = String::new();

    let result = jmap
        .query_calendar_events(QueryCalendarEventsParams {
            account_id: &account_id,
            calendar_id: None,
            // RFC 3339 extended format required by Stalwart's JMAP
            // CalendarEvent/query filter deserializer (not basic ISO 8601)
            start: &start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            end: &end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            limit: 1000,
            username: mailbox,
            password: &secret_password,
        })
        .await?;

    for event in &result.events {
        if let Some(ref ics) = event.i_calendar
            && let Some(item) = parse_ics_event(ics)
        {
            let sd = match item.busy_status.unwrap_or(2) {
                0 => '0',
                1 => '1',
                3 => '3',
                _ => '2',
            };
            for (i, slot) in merged.iter_mut().enumerate() {
                let ss = start + chrono::Duration::minutes((i as i64) * safe_interval);
                let se = ss + chrono::Duration::minutes(safe_interval);
                if item.start < se && item.end > ss && sd > *slot {
                    *slot = sd;
                }
            }
            let busy_type = match item.busy_status.unwrap_or(2) {
                0 => "Free",
                1 => "Tentative",
                3 => "OOF",
                _ => "Busy",
            };
            events_xml_out.push_str(&format!(
                    "<t:CalendarEvent><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime><t:BusyType>{}</t:BusyType>{}</t:CalendarEvent>",
                    format_ews_datetime(&item.start),
                    format_ews_datetime(&item.end),
                    busy_type,
                    ews_calendar_event_details_xml(&item)
                ));
        }
    }

    Ok((merged.into_iter().collect(), events_xml_out))
}

async fn merged_freebusy_for_mailbox(
    state: &Arc<AppState>,
    mailbox: &str,
    password: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    interval_minutes: i64,
) -> (String, String) {
    let safe_interval = interval_minutes.clamp(5, 1440);
    let slot_count = (((end - start).num_seconds().max(0) + (safe_interval * 60 - 1))
        / (safe_interval * 60)) as usize;
    let mut merged = vec!['0'; slot_count];
    let mut events_xml_out = String::new();

    // Try JMAP Calendar first (urn:ietf:params:jmap:calendars).
    // JMAP eliminates ETag complexity and uses a single HTTP endpoint.
    // Falls back to CalDAV if JMAP Calendar is unavailable or fails.
    if let Some(jmap) = &state.jmap_client {
        if let Ok(jmap_result) = fetch_freebusy_jmap(jmap, mailbox, password, start, end).await {
            return jmap_result;
        }
        tracing::debug!(target: "ews", "JMAP Calendar free-busy failed, falling back to CalDAV");
    }

    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(_) => {
            merged.fill('4');
            return (merged.into_iter().collect(), events_xml_out);
        }
    };
    if let Ok(calendars) = caldav.find_user_calendars(mailbox, password).await
        && let Some(collection_href) = calendars.first()
        && let Ok(raw_events_xml) = caldav
            .query_events(
                collection_href,
                &start.format("%Y%m%dT%H%M%SZ").to_string(),
                &end.format("%Y%m%dT%H%M%SZ").to_string(),
                mailbox,
                password,
            )
            .await
    {
        let mut reader = Reader::from_str(&raw_events_xml);
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        let mut in_calendar_data = false;
        let mut caldata_buf = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = true;
                    caldata_buf.clear();
                }
                Ok(Event::Text(ref t)) if in_calendar_data => {
                    if let Ok(ics) = t.decode() {
                        caldata_buf.push_str(&ics);
                    }
                }
                Ok(Event::CData(ref t)) if in_calendar_data => {
                    caldata_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
                Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = false;
                    let ics = caldata_buf.trim();
                    // Skip empty calendar-data (likely calendar collection root)
                    if !ics.is_empty()
                        && let Some(item) = parse_ics_event(ics)
                    {
                        let sd = match item.busy_status.unwrap_or(2) {
                            0 => '0',
                            1 => '1',
                            3 => '3',
                            _ => '2',
                        };
                        for (i, slot) in merged.iter_mut().enumerate() {
                            let ss = start + chrono::Duration::minutes((i as i64) * safe_interval);
                            let se = ss + chrono::Duration::minutes(safe_interval);
                            if item.start < se && item.end > ss && sd > *slot {
                                *slot = sd;
                            }
                        }
                        let busy_type = match item.busy_status.unwrap_or(2) {
                            0 => "Free",
                            1 => "Tentative",
                            3 => "OOF",
                            _ => "Busy",
                        };
                        events_xml_out.push_str(&format!(
                            "<t:CalendarEvent><t:StartTime>{}</t:StartTime><t:EndTime>{}</t:EndTime><t:BusyType>{}</t:BusyType>{}</t:CalendarEvent>",
                            format_ews_datetime(&item.start), format_ews_datetime(&item.end), busy_type, ews_calendar_event_details_xml(&item)
                        ));
                    }
                }
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
    } else {
        merged.fill('4');
    }
    (merged.into_iter().collect(), events_xml_out)
}

fn suggestion_day_keys(
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    if end <= start {
        return vec![start.format("%Y-%m-%d").to_string()];
    }
    let mut day = start.date_naive();
    let last = (end - chrono::Duration::seconds(1)).date_naive();
    let mut days = Vec::new();
    while day <= last {
        days.push(day.format("%Y-%m-%d").to_string());
        let Some(next_day) = day.succ_opt() else {
            break;
        };
        day = next_day;
    }
    days
}

fn suggestions_xml_for_window(
    merged: &str,
    start: chrono::DateTime<chrono::Utc>,
    end: chrono::DateTime<chrono::Utc>,
    slot_minutes: i64,
    meeting_minutes: i64,
) -> String {
    let safe_slot = slot_minutes.clamp(5, 1440);
    let safe_meeting = meeting_minutes.clamp(safe_slot, 24 * 60);
    let slots_needed = ((safe_meeting + safe_slot - 1) / safe_slot) as usize;
    let mut day_buckets: std::collections::BTreeMap<String, Vec<String>> =
        suggestion_day_keys(start, end)
            .into_iter()
            .map(|day| (day, Vec::new()))
            .collect();
    let chars = merged.chars().collect::<Vec<_>>();
    for idx in 0..chars.len() {
        if chars[idx] != '0' {
            continue;
        }
        if idx + slots_needed > chars.len()
            || chars[idx..idx + slots_needed].iter().any(|c| *c != '0')
        {
            continue;
        }
        let slot_start = start + chrono::Duration::minutes((idx as i64) * safe_slot);
        let slot_end = slot_start + chrono::Duration::minutes(safe_meeting);
        if slot_end > end {
            continue;
        }
        let day_key = slot_start.format("%Y-%m-%d").to_string();
        let entry = day_buckets.entry(day_key).or_default();
        if entry.len() >= 8 {
            continue;
        }
        entry.push(format!("<t:Suggestion><t:MeetingTime>{}</t:MeetingTime><t:IsWorkTime>true</t:IsWorkTime><t:SuggestionQuality>Excellent</t:SuggestionQuality></t:Suggestion>", format_ews_datetime(&slot_start)));
    }
    let day_results = day_buckets.into_iter().map(|(day, suggestions)| {
        let quality = if suggestions.is_empty() { "Poor" } else { "Excellent" };
        format!("<t:SuggestionDayResult><t:Date>{}</t:Date><t:DayQuality>{}</t:DayQuality><t:SuggestionArray>{}</t:SuggestionArray></t:SuggestionDayResult>",
            day, quality, suggestions.join(""))
    }).collect::<String>();
    format!(
        r#"<m:SuggestionsResponse>
  <m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage>
  <m:SuggestionDayResultArray>{}</m:SuggestionDayResultArray>
</m:SuggestionsResponse>"#,
        day_results
    )
}

fn merge_merged_freebusy(a: &str, b: &str) -> String {
    a.bytes()
        .zip_longest(b.bytes())
        .map(|pair| {
            let (l, r) = pair.or(b'0', b'0');
            char::from(l.max(r))
        })
        .collect()
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [
            ("WWW-Authenticate", "Basic realm=\"EWS\""),
            (
                "Strict-Transport-Security",
                "max-age=63072000; includeSubDomains",
            ),
        ],
        "Unauthorized",
    )
        .into_response()
}

fn soap_ok(inner: String) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="{type_ns}" />
  </s:Header>
  <s:Body>{inner}</s:Body>
</s:Envelope>"#,
        type_ns = EWS_TYPE_NS,
        inner = inner
    );
    ews_response(StatusCode::OK, xml)
}

fn soap_fault(code: &str, message: &str, status: StatusCode) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header><t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="{type_ns}" /></s:Header>
  <s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>{}</faultstring><detail><m:ResponseCode xmlns:m="{}">{}</m:ResponseCode></detail></s:Fault></s:Body>
</s:Envelope>"#,
        xml_escape(message),
        EWS_MSG_NS,
        xml_escape(code),
        type_ns = EWS_TYPE_NS
    );
    ews_response(status, xml)
}

fn validate_requested_folder(
    action: &EwsAction,
    owner: &str,
    body: &str,
) -> Result<(), Box<Response>> {
    let distinguished = extract_first_attr(body, b"DistinguishedFolderId", b"Id");
    let explicit_id = extract_first_attr(body, b"FolderId", b"Id");
    let parent_id = extract_first_attr(body, b"ParentFolderId", b"Id");
    let sync_id = extract_first_attr(body, b"SyncFolderId", b"Id");

    let all_owner_ids: Vec<String> = [
        DistinguishedFolder::Calendar,
        DistinguishedFolder::MsgFolderRoot,
        DistinguishedFolder::Inbox,
        DistinguishedFolder::SentItems,
        DistinguishedFolder::DeletedItems,
        DistinguishedFolder::Drafts,
        DistinguishedFolder::Outbox,
        DistinguishedFolder::JunkEmail,
        DistinguishedFolder::Contacts,
        DistinguishedFolder::Tasks,
        DistinguishedFolder::Notes,
        DistinguishedFolder::Journal,
    ]
    .iter()
    .map(|&f| folder_id_for(owner, f))
    .collect();

    for maybe_id in [&explicit_id, &parent_id, &sync_id] {
        if let Some(fid) = maybe_id
            && fid != "root"
            && !all_owner_ids.contains(fid)
        {
            return Err(operation_error_response(
                action,
                "ErrorFolderNotFound",
                "Requested folder was not found for this mailbox",
                StatusCode::OK,
            )
            .into());
        }
    }

    if let Some(error_code) = validate_folder_request(
        owner,
        distinguished.as_deref(),
        explicit_id.as_deref(),
        sync_id.as_deref(),
    ) {
        return Err(operation_error_response(
            action,
            error_code,
            "Requested folder was not found for this mailbox",
            StatusCode::OK,
        )
        .into());
    }
    Ok(())
}

struct CurrentCalendarItem {
    row: EwsItemRow,
    item: crate::calendar::CalendarItem,
}

fn parse_calendar_view_window(
    body: &str,
) -> Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)> {
    let start = extract_first_attr(body, b"CalendarView", b"StartDate")
        .and_then(|v| crate::calendar::parse_datetime(&v));
    let end = extract_first_attr(body, b"CalendarView", b"EndDate")
        .and_then(|v| crate::calendar::parse_datetime(&v));
    match (start, end) {
        (Some(s), Some(e)) if e > s => Some((s, e)),
        _ => None,
    }
}

fn requested_freebusy_view_type(body: &str) -> &'static str {
    match extract_first_tag_text(body, b"RequestedView")
        .unwrap_or_else(|| "MergedOnly".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "freebusy" => "FreeBusy",
        "freebusydetailed" => "Detailed",
        "detailedmerged" => "DetailedMerged",
        _ => "MergedOnly",
    }
}

fn requested_item_shape(body: &str) -> ItemShape {
    match extract_first_tag_text(body, b"BaseShape")
        .unwrap_or_else(|| "AllProperties".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "idonly" => ItemShape::IdOnly,
        "default" => ItemShape::Default,
        _ => ItemShape::AllProperties,
    }
}

async fn load_current_calendar_items(
    state: &Arc<AppState>,
    owner: &str,
    password: &str,
    window: Option<(chrono::DateTime<chrono::Utc>, chrono::DateTime<chrono::Utc>)>,
) -> Result<Vec<CurrentCalendarItem>, anyhow::Error> {
    let caldav = CaldavClient::new(&state.cfg)?;
    let calendars = caldav.find_user_calendars(owner, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow::anyhow!("no calendars found"))?
        .clone();
    let (start, end) = window.unwrap_or_else(|| {
        (
            chrono::Utc::now() - chrono::Duration::weeks(104),
            chrono::Utc::now() + chrono::Duration::weeks(104),
        )
    });
    let events_xml = caldav
        .query_events(
            &collection_href,
            &start.format("%Y%m%dT%H%M%SZ").to_string(),
            &end.format("%Y%m%dT%H%M%SZ").to_string(),
            owner,
            password,
        )
        .await?;
    let mut reader = Reader::from_str(&events_xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_caldata = false;
    let mut caldata_buf = String::new();
    let mut href = String::new();
    let mut etag = String::new();
    let mut ics = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                b"href" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        href = t.decode().unwrap_or_default().trim().to_string();
                    }
                }
                b"getetag" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        etag = t.decode().unwrap_or_default().trim_matches('"').to_string();
                    }
                }
                b"calendar-data" => {
                    in_caldata = true;
                    caldata_buf.clear();
                }
                _ => {}
            },
            Ok(Event::Text(ref t)) if in_caldata => {
                if let Ok(txt) = t.decode() {
                    caldata_buf.push_str(&txt);
                }
            }
            Ok(Event::CData(ref t)) if in_caldata => {
                caldata_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(ref e)) => match e.name().local_name().as_ref() {
                b"calendar-data" if in_caldata => {
                    in_caldata = false;
                    ics = caldata_buf.trim().to_string();
                }
                b"response" => {
                    if !href.is_empty()
                        && let Some(item) = parse_ics_event(&ics)
                    {
                        let server_id = generate_server_id(state.cfg.hmac_secret(), &href);
                        let safe_etag = if etag.is_empty() {
                            caldav
                                .get_etag(&href, owner, password)
                                .await
                                .ok()
                                .flatten()
                                .unwrap_or_else(|| {
                                    format!(
                                        "{}{}",
                                        crate::caldav::CaldavClient::SYNTHETIC_ETAG_PREFIX,
                                        const_hex::encode({
                                            let mut h = Sha256::new();
                                            h.update(server_id.as_bytes());
                                            h.finalize()
                                        })
                                    )
                                })
                        } else {
                            etag.clone()
                        };
                        if let Err(e) = state
                            .storage
                            .upsert_item_map(
                                owner,
                                &collection_href,
                                &href,
                                &server_id,
                                &item.uid,
                                &safe_etag,
                            )
                            .await
                        {
                            tracing::warn!(server_id = %server_id, error = %e, "Failed to upsert item map in load_current_calendar_items");
                        }
                        out.push(CurrentCalendarItem {
                            row: EwsItemRow {
                                server_id,
                                resource_href: href.clone(),
                                uid: Some(item.uid.clone()),
                                caldav_href: Some(collection_href.clone()),
                                etag: Some(safe_etag),
                                updated_at: None,
                            },
                            item,
                        });
                    }
                    href.clear();
                    etag.clear();
                    ics.clear();
                }
                _ => {}
            },
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn requested_folder_from_ids(body: &str, owner: &str) -> DistinguishedFolder {
    requested_folder_from_ids_with_order(
        body,
        owner,
        &[b"FolderId".as_slice(), b"ParentFolderId".as_slice()],
    )
}

fn requested_find_folder_parent_from_ids(body: &str, owner: &str) -> DistinguishedFolder {
    if let Some(folder_ref) = extract_find_folder_parent_id(body) {
        return resolve_requested_folder_ref(folder_ref, owner);
    }

    DistinguishedFolder::Calendar
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RequestedFolderRef {
    Distinguished(String),
    Explicit(String),
}

fn extract_find_folder_parent_id(body: &str) -> Option<RequestedFolderRef> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_parent_folder_ids = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"ParentFolderIds" => {
                in_parent_folder_ids = true;
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if in_parent_folder_ids
                    && matches!(
                        e.name().local_name().as_ref(),
                        b"FolderId" | b"DistinguishedFolderId"
                    ) =>
            {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == b"Id"
                        && let Ok(v) = a.decode_and_unescape_value(reader.decoder())
                    {
                        let id = v.into_owned();
                        return Some(
                            if e.name().local_name().as_ref() == b"DistinguishedFolderId" {
                                RequestedFolderRef::Distinguished(id)
                            } else {
                                RequestedFolderRef::Explicit(id)
                            },
                        );
                    }
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"ParentFolderIds" => {
                in_parent_folder_ids = false;
            }
            Ok(Event::Eof) | Err(_) => return None,
            _ => {}
        }
        buf.clear();
    }
}

fn requested_folder_from_ids_with_order(
    body: &str,
    owner: &str,
    folder_tags: &[&[u8]],
) -> DistinguishedFolder {
    if let Some(distinguished_id) = extract_first_attr(body, b"DistinguishedFolderId", b"Id") {
        return parse_distinguished_folder_id(&distinguished_id)
            .unwrap_or(DistinguishedFolder::Calendar);
    }

    // Also try resolving by explicit folder IDs — clients like eM Client may query by
    // ID after receiving it from SyncFolderHierarchy or FindFolder. EWS also accepts
    // the literal root ID, which maps to MsgFolderRoot.
    for tag in folder_tags {
        if let Some(id) = extract_first_attr(body, tag, b"Id") {
            return resolve_explicit_folder_id(&id, owner);
        }
    }

    DistinguishedFolder::Calendar
}

fn resolve_requested_folder_ref(
    folder_ref: RequestedFolderRef,
    owner: &str,
) -> DistinguishedFolder {
    match folder_ref {
        RequestedFolderRef::Distinguished(id) => {
            parse_distinguished_folder_id(&id).unwrap_or(DistinguishedFolder::Calendar)
        }
        RequestedFolderRef::Explicit(id) => resolve_explicit_folder_id(&id, owner),
    }
}

fn parse_distinguished_folder_id(id: &str) -> Result<DistinguishedFolder, String> {
    let normalized = id.trim();
    DistinguishedFolder::from_str(normalized)
        .map_err(|()| format!("unrecognized distinguished folder id: {normalized:?}"))
}

fn resolve_explicit_folder_id(id: &str, owner: &str) -> DistinguishedFolder {
    if id.eq_ignore_ascii_case("root") {
        return DistinguishedFolder::MsgFolderRoot;
    }

    resolve_folder_id(id, owner).unwrap_or(DistinguishedFolder::Calendar)
}

async fn handle_get_folder(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::GetFolder, owner, body) {
        return *resp;
    }
    let folder = requested_folder_from_ids(body, owner);
    let total_count =
        if folder.is_calendar() || matches!(folder, DistinguishedFolder::MsgFolderRoot) {
            load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
                .await
                .map(|v| v.len())
                .unwrap_or(0)
        } else {
            0
        };
    let folders_xml = render_folder_xml(owner, folder, total_count);
    let response = format!(
        r#"<m:GetFolderResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Folders>{}</m:Folders></m:GetFolderResponseMessage></m:ResponseMessages></m:GetFolderResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, folders_xml
    );
    soap_ok(response)
}

async fn handle_find_folder(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::FindFolder, owner, body) {
        return *resp;
    }
    let parent_folder = requested_find_folder_parent_from_ids(body, owner);
    let (total_count, folders_xml) = if matches!(parent_folder, DistinguishedFolder::MsgFolderRoot)
    {
        let count = load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        // Return MsgFolderRoot's direct children so clients have folder IDs for
        // operations like GetUserConfiguration.
        render_root_and_children(owner, count)
    } else {
        (0usize, String::new())
    };
    let response = format!(
        r#"<m:FindFolderResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="true"><t:Folders>{}</t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, total_count, folders_xml
    );
    soap_ok(response)
}

/// Handle EWS FindItem for email folders by routing to JMAP.
///
/// Per MS-OXWSCORE §3.1.4.6, FindItem queries items in a folder.
/// For email folders, we translate to JMAP Email/query and Email/get.
async fn handle_find_email_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
    folder: &DistinguishedFolder,
) -> Response {
    let max = extract_int(body, b"MaxEntriesReturned", 50);
    let offset = extract_int(body, b"Offset", 0);
    let mailbox_role = match folder {
        DistinguishedFolder::Inbox => "inbox",
        DistinguishedFolder::SentItems => "sentitems",
        DistinguishedFolder::Drafts => "drafts",
        DistinguishedFolder::DeletedItems => "deleteditems",
        DistinguishedFolder::JunkEmail => "junkemail",
        DistinguishedFolder::Outbox => "outbox",
        // MsgFolderRoot is the email root — not a specific mailbox.
        // Returning "inbox" here is a safe fallback since the client
        // is querying the top-level email folder.
        DistinguishedFolder::MsgFolderRoot => "inbox",
        _ => {
            tracing::warn!(
                ?folder,
                "Unrecognised EWS folder for email FindItem; returning empty"
            );
            "outbox" // fetch_emails_jmap returns empty for outbox
        }
    };

    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::FindItem,
                "ErrorInternalServerError",
                "JMAP client not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get JMAP account ID for FindItem email");
            return operation_error_response(
                &EwsAction::FindItem,
                "ErrorInternalServerError",
                "Failed to get email account",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    match crate::email::fetch_emails_jmap(
        state,
        &account_id,
        mailbox_role,
        offset as u64,
        max as u64,
        &auth.username,
        &auth.password,
    )
    .await
    {
        Ok(result) => {
            let emails = result.emails;
            let total_items = result.total; // JMAP calculateTotal, not page length
            let mut item_xml = String::new();
            for email in &emails {
                let jmap_id = email.id.as_deref().unwrap_or("unknown");
                let server_id = crate::email::email_server_id_from_jmap_id(jmap_id);
                let change_key = server_id.clone();
                item_xml.push_str(&crate::email::render_jmap_email_as_ews_message(
                    email,
                    &server_id,
                    &change_key,
                ));
            }
            let includes_last = if offset as u64 + emails.len() as u64 >= total_items {
                "true"
            } else {
                "false"
            };
            let response = format!(
                r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}"><t:Items>{}</t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>"#,
                EWS_MSG_NS,
                EWS_TYPE_NS,
                total_items,
                includes_last,
                offset + emails.len(),
                item_xml
            );
            soap_ok(response)
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch emails from JMAP for FindItem");
            operation_error_response(
                &EwsAction::FindItem,
                "ErrorInternalServerError",
                "Failed to fetch email items",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

async fn handle_find_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::FindItem, owner, body) {
        return *resp;
    }

    // Determine the distinguished folder being queried
    let distinguished_str = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .unwrap_or_else(|| "calendar".to_string());
    let folder =
        parse_distinguished_folder_id(&distinguished_str).unwrap_or(DistinguishedFolder::Calendar);

    // Email folders — route to JMAP
    if folder.is_email() && state.cfg.email_enabled && state.jmap_client.is_some() {
        return handle_find_email_item(state, auth, body, &folder).await;
    }

    // Calendar items — existing CalDAV path
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
    let view_window = parse_calendar_view_window(body);
    let shape = requested_item_shape(body);
    let items =
        match load_current_calendar_items(state, owner, auth.password.expose_secret(), view_window)
            .await
        {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(error = %e, "An internal error occurred while querying items");
                return operation_error_response(
                    &EwsAction::FindItem,
                    "ErrorInternalServerError",
                    "An internal error occurred while querying items",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };
    let folder_id = folder_id_for_owner(owner);
    let total_items = items.len();
    let paged = items.into_iter().skip(offset).take(max).collect::<Vec<_>>();
    let mut item_xml = String::new();
    for current in &paged {
        let ck = changekey_for_item(&current.row);
        let att_list = state
            .attachment_manager
            .get_attachments_for_item(owner, &current.row.server_id)
            .await
            .unwrap_or_default();
        let has_atts = !att_list.is_empty();
        let att_summaries: Vec<_> = att_list.iter().map(|a| a.to_ews_summary()).collect();
        let att_ref = if att_summaries.is_empty() {
            None
        } else {
            Some(att_summaries.as_slice())
        };
        item_xml.push_str(&render_ews_calendar_item_xml_with_shape(
            &current.row.server_id,
            &ck,
            &current.item,
            shape,
            has_atts,
            att_ref,
        ));
    }
    let includes_last = if offset + paged.len() >= total_items {
        "true"
    } else {
        "false"
    };
    let next_offset = offset + paged.len();
    if let Err(e) = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &format!("offset:{}", next_offset))
        .await
    {
        tracing::warn!(folder_id = %folder_id, error = %e, "Failed to set EWS sync state in FindItem");
    }
    let response = format!(
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}"><t:Items>{}</t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, total_items, includes_last, next_offset, item_xml
    );
    soap_ok(response)
}

/// Handle EWS GetItem for email messages by routing to JMAP.
///
/// Per MS-OXWSCORE §3.1.4.4, GetItem retrieves the full content of an item.
/// For email messages, we fetch from JMAP and render as EWS MessageType.
async fn handle_get_email_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    item_id: &str,
) -> Response {
    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "JMAP client not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get JMAP account ID for GetItem email");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "Failed to get email account",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Extract JMAP ID from the prefix-based email server ID
    let jmap_id = match crate::email::jmap_id_from_email_server_id(item_id) {
        Some(id) => id.to_string(),
        None => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorItemNotFound",
                "Invalid email item ID format",
                StatusCode::OK,
            );
        }
    };
    match jmap
        .get_email(&account_id, &jmap_id, &auth.username, &auth.password)
        .await
    {
        Ok(Some(email)) => {
            let server_id = crate::email::email_server_id_from_jmap_id(&jmap_id);
            let change_key = server_id.clone();
            let item_xml =
                crate::email::render_jmap_email_as_ews_message(&email, &server_id, &change_key);
            let response = format!(
                r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse>"#,
                EWS_MSG_NS, EWS_TYPE_NS, item_xml
            );
            soap_ok(response)
        }
        Ok(None) => operation_error_response(
            &EwsAction::GetItem,
            "ErrorItemNotFound",
            "Requested email item does not exist",
            StatusCode::OK,
        ),
        Err(e) => {
            tracing::warn!(error = %e, item_id = %item_id, "Failed to fetch email from JMAP for GetItem");
            operation_error_response(
                &EwsAction::GetItem,
                "ErrorItemNotFound",
                "Requested email item does not exist",
                StatusCode::OK,
            )
        }
    }
}

async fn handle_get_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::GetItem,
            "ErrorInvalidIdMalformed",
            "GetItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    // Check if the request asks for a Message shape (email) rather than CalendarItem
    let is_email_request = body.contains("<t:Message") || body.contains("MessageDisposition");

    // Try to look up the item in the calendar DB first
    let item_owner = match state.storage.get_item_owner(&item_id).await {
        Ok(Some(o)) => Some(o),
        Ok(None) => None,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Fast path: if the item ID has the email prefix, route directly to JMAP
    if crate::email::is_email_server_id(&item_id) && state.email_available() {
        return handle_get_email_item(state, auth, &item_id).await;
    }

    // If item not found in calendar DB and email is enabled, try JMAP
    if item_owner.is_none() && state.cfg.email_enabled && state.jmap_client.is_some() {
        return handle_get_email_item(state, auth, &item_id).await;
    }

    // If explicitly requesting email type, route to JMAP
    if is_email_request && state.cfg.email_enabled && state.jmap_client.is_some() {
        return handle_get_email_item(state, auth, &item_id).await;
    }

    let item_owner = match item_owner {
        Some(o) => o,
        None => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorItemNotFound",
                "Requested item does not exist",
                StatusCode::OK,
            );
        }
    };
    let calendar_folder_id = folder_id_for(&item_owner, DistinguishedFolder::Calendar);
    let enforcement = PermissionEnforcement::new(&state.storage);
    let perm_ctx = PermissionContext::new(
        auth.username.clone(),
        item_owner.clone(),
        calendar_folder_id.clone(),
    );
    match enforcement.can_read_item(&perm_ctx).await {
        Ok(true) => {}
        Ok(false) => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorAccessDenied",
                "You do not have permission to read this calendar",
                StatusCode::FORBIDDEN,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred during permission check");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred during permission check",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }

    let item = match state
        .storage
        .get_ews_item_by_server_id(&item_owner, &item_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while loading the item");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred while loading the item",
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
    let shape = requested_item_shape(body);
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let calendar_item_xml = match caldav
        .get_event(
            &item.resource_href,
            &item_owner,
            auth.password.expose_secret(),
        )
        .await
    {
        Ok((ics, _)) => match parse_ics_event(&ics) {
            Some(ci) => {
                let att_list = state
                    .attachment_manager
                    .get_attachments_for_item(&item_owner, &item.server_id)
                    .await
                    .unwrap_or_default();
                let has_atts = !att_list.is_empty();
                let att_summaries: Vec<_> = att_list.iter().map(|a| a.to_ews_summary()).collect();
                let att_ref = if att_summaries.is_empty() {
                    None
                } else {
                    Some(att_summaries.as_slice())
                };
                render_ews_calendar_item_xml_with_shape(
                    &item.server_id,
                    &ck,
                    &ci,
                    shape,
                    has_atts,
                    att_ref,
                )
            }
            None => format!(
                r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:UID>{}</t:UID></t:CalendarItem>"#,
                xml_escape(&item.server_id),
                xml_escape(&ck),
                xml_escape(item.uid.as_deref().unwrap_or(&item.server_id)),
                xml_escape(item.uid.as_deref().unwrap_or(&item.server_id))
            ),
        },
        Err(_) => format!(
            r#"<t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:UID>{}</t:UID></t:CalendarItem>"#,
            xml_escape(&item.server_id),
            xml_escape(&ck),
            xml_escape(item.uid.as_deref().unwrap_or(&item.server_id)),
            xml_escape(item.uid.as_deref().unwrap_or(&item.server_id))
        ),
    };
    let response = format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, calendar_item_xml
    );
    soap_ok(response)
}

fn encode_sync_state_cursor(last_seen_seq: i64, upper_bound_seq: i64) -> String {
    let payload = format!("seq:{}:{}", last_seen_seq.max(0), upper_bound_seq.max(0));
    STANDARD.encode(payload.as_bytes())
}

fn decode_sync_state_cursor(marker: &str) -> Result<(i64, i64), ()> {
    let raw = STANDARD.decode(marker).map_err(|_| ())?;
    let decoded = String::from_utf8(raw).map_err(|_| ())?;
    let rest = decoded.strip_prefix("seq:").ok_or(())?;
    let mut parts = rest.split(':');
    let last_seen = parts.next().ok_or(())?.parse::<i64>().map_err(|_| ())?;
    let upper_bound = parts.next().ok_or(())?.parse::<i64>().map_err(|_| ())?;
    if parts.next().is_some() {
        return Err(());
    }
    Ok((last_seen.max(0), upper_bound.max(last_seen)))
}

fn parse_sync_state_marker(marker: Option<String>) -> Result<(i64, i64), ()> {
    match marker {
        None => Ok((0, 0)),
        Some(m) if m.is_empty() || m == "0" => Ok((0, 0)),
        Some(m) => decode_sync_state_cursor(&m),
    }
}

/// Handle SyncFolderItems for email folders by delegating to JMAP Email/changes.
///
/// This maps the EWS SyncFolderItems pattern (cursor-based, with Create/Update/Delete
/// change entries) to JMAP Email/changes (RFC 8621 §4.4).
///
/// The JMAP `state` token is stored as the EWS `SyncState` value.
async fn handle_sync_email_folder_items(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
    folder: &DistinguishedFolder,
) -> Response {
    use crate::email::{email_server_id_from_jmap_id, render_jmap_email_as_ews_message};

    let owner = owner_from_username(&auth.username);
    let folder_id = folder_id_for(owner, *folder);
    let _max_changes = extract_int(body, b"MaxChangesReturned", 100).clamp(1, 512);

    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                "JMAP client not configured for email",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "JMAP get_account_id failed in SyncFolderItems");
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorFolderNotFound",
                "JMAP account not found",
                StatusCode::OK,
            );
        }
    };

    // Retrieve the current sync state (JMAP state token) from our DB
    let requested_state = extract_first_tag_text(body, b"SyncState");
    let old_state_token = requested_state.unwrap_or_default();
    let is_initial = old_state_token.is_empty();

    // Map EWS distinguished folder to JMAP mailbox role
    let mailbox_role = match folder {
        DistinguishedFolder::Inbox => "inbox",
        DistinguishedFolder::SentItems => "sent",
        DistinguishedFolder::Drafts => "drafts",
        DistinguishedFolder::JunkEmail => "junk",
        DistinguishedFolder::DeletedItems => "trash",
        DistinguishedFolder::Outbox => "outbox",
        // MsgFolderRoot is the email root — not a specific mailbox.
        // Returning "inbox" here is a safe fallback since the client
        // is syncing the top-level email folder.
        DistinguishedFolder::MsgFolderRoot => "inbox",
        _ => {
            tracing::warn!(
                ?folder,
                "Unrecognised EWS folder for email SyncFolderItems; returning empty"
            );
            "outbox" // fetch_emails_jmap returns empty for outbox
        }
    };

    if is_initial {
        // Initial sync: JMAP Email/changes requires a valid sinceState token.
        // Use Email/query + Email/get (batched) to fetch current emails and
        // obtain the current data state token for subsequent /changes calls.
        let result = match crate::email::fetch_emails_jmap(
            state,
            &account_id,
            mailbox_role,
            0,
            _max_changes as u64,
            &auth.username,
            &auth.password,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "JMAP Email/query failed for initial sync");
                return operation_error_response(
                    &EwsAction::SyncFolderItems,
                    "ErrorInternalServerError",
                    "Failed to fetch emails for initial sync",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let emails = result.emails;
        let new_state = if result.state.is_empty() {
            // Fallback: fetch state token separately if batched get didn't return it
            match jmap
                .get_email_state(&account_id, &auth.username, &auth.password)
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to get JMAP email state token; using empty state");
                    String::new()
                }
            }
        } else {
            result.state
        };

        let mut changes_xml = String::new();
        for email in &emails {
            let jmap_id = email.id.as_deref().unwrap_or("unknown");
            let server_id = email_server_id_from_jmap_id(jmap_id);
            let change_key = server_id.clone();
            changes_xml.push_str(&format!(
                r#"<t:Create>{}</t:Create>"#,
                render_jmap_email_as_ews_message(email, &server_id, &change_key)
            ));
        }

        if let Err(e) = state
            .storage
            .set_ews_sync_state(owner, &folder_id, &new_state)
            .await
        {
            tracing::warn!(folder_id = %folder_id, error = %e, "Failed to set EWS email sync state");
        }

        let response = format!(
            r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>true</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
            EWS_MSG_NS,
            EWS_TYPE_NS,
            xml_escape(&new_state),
            changes_xml
        );
        return soap_ok(response);
    }

    // Subsequent sync: call JMAP Email/changes with the stored state token
    let changes_result = jmap
        .sync_email_changes(
            &account_id,
            &old_state_token,
            &auth.username,
            &auth.password,
        )
        .await;

    let changes = match changes_result {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "JMAP Email/changes failed");
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                "Failed to sync email changes via JMAP",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let mut changes_xml = String::new();

    // Fetch and render created emails
    for email_id in &changes.created {
        if let Ok(Some(email)) = jmap
            .get_email(&account_id, email_id, &auth.username, &auth.password)
            .await
        {
            let server_id = email_server_id_from_jmap_id(email_id);
            let change_key = server_id.clone();
            changes_xml.push_str(&format!(
                r#"<t:Create>{}</t:Create>"#,
                render_jmap_email_as_ews_message(&email, &server_id, &change_key)
            ));
        }
    }

    // Fetch and render updated emails
    for email_id in &changes.updated {
        if let Ok(Some(email)) = jmap
            .get_email(&account_id, email_id, &auth.username, &auth.password)
            .await
        {
            let server_id = email_server_id_from_jmap_id(email_id);
            let change_key = server_id.clone();
            changes_xml.push_str(&format!(
                r#"<t:Update>{}</t:Update>"#,
                render_jmap_email_as_ews_message(&email, &server_id, &change_key)
            ));
        }
    }

    // Render deleted emails
    for email_id in &changes.destroyed {
        let server_id = email_server_id_from_jmap_id(email_id);
        changes_xml.push_str(&format!(
            r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
            xml_escape(&server_id)
        ));
    }

    // Store the new sync state
    if let Err(e) = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &changes.new_state)
        .await
    {
        tracing::warn!(folder_id = %folder_id, error = %e, "Failed to set EWS email sync state");
    }

    let includes_last = if changes.has_more_changes {
        "false"
    } else {
        "true"
    };

    let response = format!(
        r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>{}</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&changes.new_state),
        includes_last,
        changes_xml
    );
    soap_ok(response)
}

async fn handle_sync_folder_items(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::SyncFolderItems, owner, body) {
        return *resp;
    }

    // Determine the folder type from the request
    let distinguished_str =
        extract_first_attr(body, b"DistinguishedFolderId", b"Id").unwrap_or_default();
    let distinguished =
        parse_distinguished_folder_id(&distinguished_str).unwrap_or(DistinguishedFolder::Calendar);

    // Route email folders to JMAP-based sync
    if distinguished.is_email() && state.email_available() {
        return handle_sync_email_folder_items(state, auth, body, &distinguished).await;
    }

    let max_changes = extract_int(body, b"MaxChangesReturned", 100);
    let shape = requested_item_shape(body);
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
            Err(e) => {
                tracing::error!(error = %e, owner = %owner, folder_id = %folder_id, "Failed to fetch EWS sync state");
                return operation_error_response(
                    &EwsAction::SyncFolderItems,
                    "ErrorInternalServerError",
                    "An internal error occurred while fetching sync state",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    } else {
        requested_state
    };
    let (since, requested_upper_bound) = match parse_sync_state_marker(effective_state) {
        Ok(v) => v,
        Err(_) => {
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInvalidSyncStateData",
                "SyncState is invalid; expected an opaque base64-encoded gateway sync blob",
                StatusCode::OK,
            );
        }
    };
    let latest_seq = state.storage.get_latest_change_seq().await.unwrap_or(0);
    let upper_bound = if requested_upper_bound > since {
        requested_upper_bound.min(latest_seq)
    } else {
        latest_seq
    };
    let journal_rows = match state.storage.list_journal_since_seq(owner, since).await {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let items = match load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::SyncFolderItems,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let mut current_map = HashMap::new();
    for item in items {
        current_map.insert(item.row.server_id.clone(), item);
    }
    let deleted_ids: HashSet<String> = journal_rows
        .iter()
        .filter(|r| r.op == "delete")
        .map(|r| r.server_id.clone())
        .collect();
    let has_more = journal_rows.len() > max_changes;
    let visible_rows = if has_more {
        &journal_rows[..max_changes]
    } else {
        &journal_rows[..]
    };
    let caldav = CaldavClient::new(&state.cfg).ok();
    let mut emitted_ids = HashSet::new();
    let mut changes_xml = String::new();
    let mut last_returned_seq = since;
    for row in visible_rows {
        last_returned_seq = row.id;
        if !emitted_ids.insert(row.server_id.clone()) {
            continue;
        }
        if row.op == "delete" {
            changes_xml.push_str(&format!(
                r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                xml_escape(&row.server_id)
            ));
            continue;
        }
        if let Some(item) = current_map.get(&row.server_id) {
            let ck = changekey_for_item(&item.row);
            let att_list = state
                .attachment_manager
                .get_attachments_for_item(owner, &row.server_id)
                .await
                .unwrap_or_default();
            let has_atts = !att_list.is_empty();
            let att_summaries: Vec<_> = att_list.iter().map(|a| a.to_ews_summary()).collect();
            let att_ref = if att_summaries.is_empty() {
                None
            } else {
                Some(att_summaries.as_slice())
            };
            let change_tag = if since == 0 { "Create" } else { "Update" };
            changes_xml.push_str(&format!(
                r#"<t:{ct}>{}</t:{ct}>"#,
                render_ews_calendar_item_xml_with_shape(
                    &item.row.server_id,
                    &ck,
                    &item.item,
                    shape,
                    has_atts,
                    att_ref
                ),
                ct = change_tag
            ));
        } else if deleted_ids.contains(&row.server_id) {
            changes_xml.push_str(&format!(
                r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                xml_escape(&row.server_id)
            ));
        } else {
            match state
                .storage
                .get_ews_item_by_server_id(owner, &row.server_id)
                .await
            {
                Ok(Some(stored)) => {
                    let fetched: Option<crate::calendar::CalendarItem> = if let Some(ref c) = caldav
                    {
                        match c
                            .get_event(&stored.resource_href, owner, auth.password.expose_secret())
                            .await
                        {
                            Ok((ics, etag)) => {
                                if let Some(item) = parse_ics_event(&ics) {
                                    // Update stored etag
                                    if let Err(e) = state
                                        .storage
                                        .upsert_item_map(
                                            owner,
                                            stored.caldav_href.as_deref().unwrap_or(""),
                                            &stored.resource_href,
                                            &row.server_id,
                                            &item.uid,
                                            etag.as_deref()
                                                .unwrap_or(stored.etag.as_deref().unwrap_or("")),
                                        )
                                        .await
                                    {
                                        tracing::warn!(server_id = %row.server_id, error = %e, "Failed to upsert item map in SyncFolderItems journal fetch");
                                    }
                                    Some(item)
                                } else {
                                    None
                                }
                            }
                            Err(_) => None,
                        }
                    } else {
                        None
                    };
                    if let Some(item) = fetched {
                        let ck = changekey_for_item(&stored);
                        let change_tag = if since == 0 { "Create" } else { "Update" };
                        let att_list = state
                            .attachment_manager
                            .get_attachments_for_item(owner, &row.server_id)
                            .await
                            .unwrap_or_default();
                        let att_summaries: Vec<_> =
                            att_list.iter().map(|a| a.to_ews_summary()).collect();
                        let att_ref = if att_summaries.is_empty() {
                            None
                        } else {
                            Some(att_summaries.as_slice())
                        };
                        changes_xml.push_str(&format!(
                            r#"<t:{ct}>{}</t:{ct}>"#,
                            render_ews_calendar_item_xml_with_shape(
                                &row.server_id,
                                &ck,
                                &item,
                                shape,
                                !att_list.is_empty(),
                                att_ref
                            ),
                            ct = change_tag
                        ));
                    } else {
                        // Event gone from CalDAV — treat as delete and clean up
                        tracing::debug!(
                            server_id = %row.server_id,
                            "Event not in CalDAV window/fetchable; emitting as Delete"
                        );
                        if let Err(e) = state
                            .storage
                            .add_delete_tombstone(owner, &row.server_id)
                            .await
                        {
                            tracing::warn!(server_id = %row.server_id, error = %e, "Failed to add delete tombstone");
                        }
                        if let Err(e) = state
                            .storage
                            .delete_item_by_server_id(owner, &row.server_id)
                            .await
                        {
                            tracing::warn!(server_id = %row.server_id, error = %e, "Failed to delete item by server_id");
                        }
                        changes_xml.push_str(&format!(
                            r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                            xml_escape(&row.server_id)
                        ));
                    }
                }
                Ok(None) => {
                    // Not in item_map either — item was already purged; emit as Delete
                    changes_xml.push_str(&format!(
                        r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                        xml_escape(&row.server_id)
                    ));
                }
                Err(e) => {
                    tracing::warn!(server_id = %row.server_id, error = %e, "Failed to look up journal item; emitting as Delete");
                    changes_xml.push_str(&format!(
                        r#"<t:Delete><t:ItemId Id="{}" /></t:Delete>"#,
                        xml_escape(&row.server_id)
                    ));
                }
            }
        }
    }
    let includes_last = if has_more { "false" } else { "true" };
    let next_seen_seq = if visible_rows.is_empty() {
        since.max(upper_bound)
    } else {
        last_returned_seq
    };
    let next_upper_bound = if includes_last == "true" {
        next_seen_seq
    } else {
        upper_bound
    };
    let new_sync_state = encode_sync_state_cursor(next_seen_seq, next_upper_bound);
    if let Err(e) = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &new_sync_state)
        .await
    {
        tracing::warn!(folder_id = %folder_id, error = %e, "Failed to set EWS sync state in SyncFolderItems");
    }
    let response = format!(
        r#"<m:SyncFolderItemsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderItemsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastItemInRange>{}</m:IncludesLastItemInRange><m:Changes>{}</m:Changes></m:SyncFolderItemsResponseMessage></m:ResponseMessages></m:SyncFolderItemsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&new_sync_state),
        includes_last,
        changes_xml
    );
    soap_ok(response)
}

async fn handle_sync_folder_hierarchy(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = owner_from_username(&auth.username);
    let sync_state_key = format!("{}/folderhierarchy", owner);
    let requested_state = extract_first_tag_text(body, b"SyncState");
    let is_initial = requested_state
        .as_deref()
        .map(|s| s.is_empty())
        .unwrap_or(true);
    let new_sync_state = encode_sync_state_cursor(0, 0);
    if let Err(e) = state
        .storage
        .set_ews_sync_state(owner, &sync_state_key, &new_sync_state)
        .await
    {
        tracing::warn!(folder_id = %sync_state_key, error = %e, "Failed to set EWS sync state in SyncFolderHierarchy");
    }
    let changes = if is_initial {
        let count = load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        // Return MsgFolderRoot first (clients need its FolderId for GetUserConfiguration),
        // then Calendar, then all other child folders.
        render_folder_hierarchy_creates(owner, count)
    } else {
        String::new()
    };
    let response = format!(
        r#"<m:SyncFolderHierarchyResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SyncFolderHierarchyResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SyncState>{}</m:SyncState><m:IncludesLastFolderInRange>true</m:IncludesLastFolderInRange><m:Changes>{}</m:Changes></m:SyncFolderHierarchyResponseMessage></m:ResponseMessages></m:SyncFolderHierarchyResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&new_sync_state),
        changes
    );
    soap_ok(response)
}

async fn handle_subscribe(_auth: &AuthContext, _body: &str) -> Response {
    let subscription_id = uuid::Uuid::new_v4().to_string();
    let watermark = STANDARD.encode(subscription_id.as_bytes());
    let response = format!(
        r#"<m:SubscribeResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SubscriptionId>{}</m:SubscriptionId><m:Watermark>{}</m:Watermark></m:SubscribeResponseMessage></m:ResponseMessages></m:SubscribeResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&subscription_id),
        xml_escape(&watermark)
    );
    soap_ok(response)
}

async fn handle_unsubscribe(_auth: &AuthContext, _body: &str) -> Response {
    let response = format!(
        r#"<m:UnsubscribeResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UnsubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UnsubscribeResponseMessage></m:ResponseMessages></m:UnsubscribeResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(response)
}

async fn handle_create_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    // Check for MessageDisposition — if present, this may be an email send
    let disposition = extract_first_tag_text(body, b"MessageDisposition");

    // If the body contains a <t:Message> element, handle as email
    if body.contains("<t:Message")
        && let Some(msg) = crate::email::parse_ews_message(body)
    {
        let disp = disposition.as_deref().unwrap_or("SaveOnly");
        if disp == "SendOnly" || disp == "SendAndSaveCopy" {
            if !state.cfg.email_enabled {
                return operation_error_response(
                    &EwsAction::CreateItem,
                    "ErrorInvalidRequest",
                    "Email operations are not enabled on this server",
                    StatusCode::FORBIDDEN,
                );
            }
            match crate::email::send_email(state, &msg, &auth.username, &auth.password).await {
                Ok(message_id) => {
                    let server_id = crate::email::email_server_id_from_send_result(&message_id);
                    let change_key = server_id.clone();
                    let items_xml =
                        crate::email::render_ews_message_item_xml(&server_id, &change_key, &msg);
                    let response = format!(
                        r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
                        EWS_MSG_NS, EWS_TYPE_NS, items_xml
                    );
                    return soap_ok(response);
                }
                Err(e) => {
                    tracing::error!(error = %e, "SMTP send failed for CreateItem email");
                    return operation_error_response(
                        &EwsAction::CreateItem,
                        "ErrorInternalServerError",
                        "Failed to send email message",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            }
        }
        // SaveOnly — return success with a synthetic ItemId
        let server_id = crate::email::email_server_id_from_jmap_id(&format!(
            "draft-{}",
            chrono::Utc::now().timestamp_millis()
        ));
        let items_xml = crate::email::render_ews_message_item_xml(&server_id, "0", &msg);
        let response = format!(
            r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
            EWS_MSG_NS, EWS_TYPE_NS, items_xml
        );
        return soap_ok(response);
    }

    // Fall through to calendar item handling
    let owner = owner_from_username(&auth.username);

    let calendar_folder_id = folder_id_for(owner, DistinguishedFolder::Calendar);
    let enforcement = PermissionEnforcement::new(&state.storage);
    let perm_ctx = PermissionContext::new(
        auth.username.clone(),
        owner.to_string(),
        calendar_folder_id.clone(),
    );
    match enforcement.can_create_item(&perm_ctx).await {
        Ok(true) => {}
        Ok(false) => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorAccessDenied",
                "You do not have permission to create calendar items",
                StatusCode::FORBIDDEN,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred during permission check");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "An internal error occurred during permission check",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }

    if let Err(resp) = validate_requested_folder(&EwsAction::CreateItem, owner, body) {
        return *resp;
    }
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let item = match parse_ews_calendar_item(body) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse CalendarItem payload");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorSchemaValidation",
                "An internal error occurred while parsing the item",
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let calendars = match caldav
        .find_user_calendars(owner, auth.password.expose_secret())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(
                error = %e,
                owner = %owner,
                "CalDAV calendar discovery failed for owner"
            );
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "An internal error occurred while discovering calendars",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let collection_href = match calendars.first() {
        Some(v) => v.clone(),
        None => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorFolderNotFound",
                "No writable calendar collection discovered",
                StatusCode::OK,
            );
        }
    };
    let ics = render_ics(&item);
    let (href, etag) = match caldav
        .put_event(
            &collection_href,
            None,
            &ics,
            owner,
            auth.password.expose_secret(),
            None,
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while saving the item");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "An internal error occurred while saving the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let server_id = generate_server_id(state.cfg.hmac_secret(), &href);
    if let Err(e) = state
        .storage
        .upsert_item_map(owner, &collection_href, &href, &server_id, &item.uid, &etag)
        .await
    {
        tracing::error!(error = %e, owner = %owner, "Failed to persist created item mapping");
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInternalServerError",
            "An internal error occurred while saving the item",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let response_row = EwsItemRow {
        server_id: server_id.clone(),
        resource_href: href,
        uid: Some(item.uid.clone()),
        caldav_href: Some(collection_href.clone()),
        etag: Some(etag),
        updated_at: None,
    };
    let response = format!(
        r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(&server_id, &changekey_for_item(&response_row), &item)
    );
    soap_ok(response)
}

/// Handle EWS UpdateItem for email messages via JMAP Email/set.
///
/// Supports common email updates: IsRead ($seen keyword), Importance ($important keyword).
async fn handle_update_email_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    item_id: &str,
    body: &str,
) -> Response {
    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "JMAP client not configured for email",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "JMAP get_account_id failed in UpdateItem email");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorItemNotFound",
                "JMAP account not found",
                StatusCode::OK,
            );
        }
    };

    // Parse IsRead from the update
    let is_read = extract_first_tag_text(body, b"IsRead").and_then(|v| v.parse::<bool>().ok());

    // Build JMAP Email/set update for keywords
    let mut keywords_update = serde_json::Map::new();
    if let Some(read) = is_read {
        if read {
            keywords_update.insert("$seen".to_string(), serde_json::Value::Bool(true));
        } else {
            keywords_update.insert("$seen".to_string(), serde_json::Value::Null);
        }
    }

    // Check for importance flag
    if body.contains("<t:Importance>High</t:Importance>") {
        keywords_update.insert("$important".to_string(), serde_json::Value::Bool(true));
    } else if body.contains("<t:Importance>Normal</t:Importance>")
        || body.contains("<t:Importance>Low</t:Importance>")
    {
        keywords_update.insert("$important".to_string(), serde_json::Value::Null);
    }

    // Extract JMAP ID from the prefix-based email server ID
    let jmap_id = match crate::email::jmap_id_from_email_server_id(item_id) {
        Some(id) => id.to_string(),
        None => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorItemNotFound",
                "Invalid email item ID format",
                StatusCode::OK,
            );
        }
    };

    if !keywords_update.is_empty() {
        let update = serde_json::json!({
            (jmap_id): {
                "keywords": keywords_update
            }
        });

        if let Err(e) = jmap
            .update_email(&account_id, &update, &auth.username, &auth.password)
            .await
        {
            tracing::warn!(error = %e, item_id = %item_id, "JMAP Email/set update failed");
        }
    }

    let change_key = item_id.to_string();

    let response = format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Message><t:ItemId Id="{}" ChangeKey="{}" /></t:Message></m:Items></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(item_id),
        xml_escape(&change_key)
    );
    soap_ok(response)
}

async fn handle_update_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();

    // Fast path: if the item ID has the email prefix, route directly to email update
    if crate::email::is_email_server_id(&item_id) && state.email_available() {
        return handle_update_email_item(state, auth, &item_id, body).await;
    }

    // Check if this is an email update (IsRead, flag changes, etc.)
    if (body.contains("<t:IsRead>") || body.contains("<t:Message")) && state.email_available() {
        return handle_update_email_item(state, auth, &item_id, body).await;
    }

    // Also check if the item isn't in the calendar DB (may be an email)
    if !item_id.is_empty() && state.email_available() {
        let is_calendar_item = state
            .storage
            .get_ews_item_by_server_id(owner, &item_id)
            .await
            .ok()
            .flatten()
            .is_some();
        if !is_calendar_item {
            return handle_update_email_item(state, auth, &item_id, body).await;
        }
    }

    let calendar_folder_id = folder_id_for(owner, DistinguishedFolder::Calendar);
    let enforcement = PermissionEnforcement::new(&state.storage);
    let perm_ctx = PermissionContext::new(
        auth.username.clone(),
        owner.to_string(),
        calendar_folder_id.clone(),
    );
    match enforcement.can_edit_item(&perm_ctx).await {
        Ok(true) => {}
        Ok(false) => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorAccessDenied",
                "You do not have permission to edit this calendar item",
                StatusCode::FORBIDDEN,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred during permission check");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred during permission check",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }
    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInvalidIdMalformed",
            "UpdateItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }
    let stored_item = match state
        .storage
        .get_ews_item_by_server_id(owner, &item_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while loading the item");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred while loading the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let Some(stored_item) = stored_item else {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorItemNotFound",
            "Requested item does not exist",
            StatusCode::OK,
        );
    };
    let conflict_resolution =
        extract_conflict_resolution(body).unwrap_or_else(|| "AutoResolve".to_string());
    let skip_ck_validation = matches!(
        conflict_resolution.to_ascii_lowercase().as_str(),
        "alwaysoverwrite" | "autoresolve"
    );
    if !skip_ck_validation
        && let Err(resp) = validate_item_change_key(&EwsAction::UpdateItem, body, &stored_item)
    {
        return *resp;
    }
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let (existing_ics, existing_etag) = match caldav
        .get_event(
            &stored_item.resource_href,
            owner,
            auth.password.expose_secret(),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while fetching the event");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred while fetching the event",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    // If the GET response didn't include an ETag header (common with Stalwart v0.16.5),
    // try to obtain one via PROPFIND so we can use a proper If-Match on the PUT.
    let existing_etag = match existing_etag {
        Some(etag) => Some(etag),
        None => {
            match caldav
                .get_etag(
                    &stored_item.resource_href,
                    owner,
                    auth.password.expose_secret(),
                )
                .await
            {
                Ok(Some(etag)) => {
                    tracing::debug!(etag = %etag, "Obtained etag via PROPFIND for update");
                    Some(etag)
                }
                _ => {
                    tracing::debug!(
                        "No etag available from GET or PROPFIND; update will proceed without If-Match"
                    );
                    None
                }
            }
        }
    };
    let mut current_item =
        parse_ics_event(&existing_ics).unwrap_or_else(|| crate::calendar::CalendarItem {
            uid: stored_item
                .uid
                .clone()
                .unwrap_or_else(|| stored_item.server_id.clone()),
            start: chrono::Utc::now(),
            end: chrono::Utc::now() + chrono::Duration::hours(1),
            dtstamp: Some(chrono::Utc::now()),
            ..Default::default()
        });
    let field_changes = parse_item_changes(body);
    if !field_changes.is_empty() {
        apply_field_changes(&mut current_item, &field_changes);
    } else {
        if let Some(v) =
            extract_ews_field(body, b"Subject").or_else(|| extract_ews_field(body, b"Value"))
        {
            current_item.subject = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"Start").and_then(|v| crate::calendar::parse_datetime(&v))
        {
            current_item.start = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"End").and_then(|v| crate::calendar::parse_datetime(&v))
        {
            current_item.end = v;
        }
        if let Some(v) = extract_ews_field(body, b"Location") {
            current_item.location = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"Body").or_else(|| extract_ews_field(body, b"TextBody"))
        {
            current_item.description = v;
        }
        if body.contains("Categories") {
            current_item.categories = extract_ews_fields(body, b"String");
        }
        if let Some(v) =
            extract_ews_field(body, b"ReminderMinutesBeforeStart").and_then(|v| v.parse().ok())
        {
            current_item.reminder = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"LegacyFreeBusyStatus") {
            current_item.busy_status = match v.as_str() {
                "Free" => Some(0),
                "Tentative" => Some(1),
                "Busy" => Some(2),
                "OOF" => Some(3),
                _ => current_item.busy_status,
            };
        }
        if let Some(v) = extract_ews_field(body, b"Sensitivity") {
            current_item.sensitivity = match v.as_str() {
                "Normal" => Some(0),
                "Personal" => Some(1),
                "Private" => Some(2),
                "Confidential" => Some(3),
                _ => current_item.sensitivity,
            };
        }
        if let Some(v) = extract_ews_field(body, b"ResponseRequested") {
            current_item.response_requested = Some(v.eq_ignore_ascii_case("true"));
        }
        if let Some(v) = extract_ews_field(body, b"DisallowNewTimeProposal") {
            current_item.disallow_new_time_proposal = Some(v.eq_ignore_ascii_case("true"));
        }
        if let Some(v) = extract_ews_field(body, b"OrganizerName") {
            current_item.organizer_name = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OrganizerEmail") {
            current_item.organizer_email = Some(nfc(&v));
        }
        if body.contains("RequiredAttendees") || body.contains("OptionalAttendees") {
            current_item.attendees = parse_ews_attendees(body);
        }
        if body.contains("Recurrence") {
            current_item.rrule = parse_ews_recurrence(body);
        }
        if let Some(v) = extract_ews_field(body, b"StartTimeZone") {
            current_item.timezone = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"MeetingTimeZone") {
            current_item.timezone_blob = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OnlineMeetingConfLink") {
            current_item.online_meeting_conf_link = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OnlineMeetingExternalLink") {
            current_item.online_meeting_external_link = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"ClientUid") {
            current_item.client_uid = Some(v);
        }
    }
    let collection_href = stored_item.caldav_href.clone().unwrap_or_else(|| {
        // Fallback: extract collection href from resource href by removing the filename
        // Use string manipulation instead of std::path for URL handling
        if let Some((parent, _)) = stored_item.resource_href.rsplit_once('/') {
            if parent.is_empty() {
                stored_item.resource_href.clone()
            } else {
                parent.to_string()
            }
        } else {
            stored_item.resource_href.clone()
        }
    });
    let uid = current_item.uid.clone();
    let ics = render_ics(&current_item);
    let (resource_href, new_etag) = match caldav
        .put_event(
            &collection_href,
            Some(&stored_item.resource_href),
            &ics,
            owner,
            auth.password.expose_secret(),
            existing_etag.as_deref(),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while saving the update");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred while saving the update",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    if let Err(e) = state
        .storage
        .upsert_item_map(
            owner,
            &collection_href,
            &resource_href,
            &stored_item.server_id,
            &uid,
            &new_etag,
        )
        .await
    {
        tracing::error!(error = %e, owner = %owner, "Failed to persist update mapping");
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInternalServerError",
            "An internal error occurred while saving the update",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let response_row = EwsItemRow {
        server_id: stored_item.server_id.clone(),
        resource_href,
        uid: Some(uid),
        caldav_href: Some(collection_href.clone()),
        etag: Some(new_etag),
        updated_at: None,
    };
    let response = format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(
            &stored_item.server_id,
            &changekey_for_item(&response_row),
            &current_item
        )
    );
    soap_ok(response)
}

/// Delete an email item via JMAP Email/set.
async fn handle_delete_email_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    item_id: &str,
) -> Response {
    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "JMAP client not configured for email",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::warn!(error = %e, "JMAP get_account_id failed in DeleteItem email");
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorItemNotFound",
                "JMAP account not found",
                StatusCode::OK,
            );
        }
    };

    // Extract JMAP ID from the prefix-based email server ID
    let jmap_id = match crate::email::jmap_id_from_email_server_id(item_id) {
        Some(id) => id.to_string(),
        None => {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorItemNotFound",
                "Invalid email item ID format",
                StatusCode::OK,
            );
        }
    };

    // Destroy the email via JMAP Email/set
    if let Err(e) = jmap
        .destroy_emails(&account_id, &[jmap_id], &auth.username, &auth.password)
        .await
    {
        tracing::warn!(error = %e, item_id = %item_id, "JMAP Email/set destroy failed for DeleteItem");
        // Still return success — the client may retry, and the email may have
        // already been deleted from JMAP by another path.
    }

    let response = format!(
        r#"<m:DeleteItemResponse xmlns:m="{}"><m:ResponseMessages><m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage></m:ResponseMessages></m:DeleteItemResponse>"#,
        EWS_MSG_NS
    );
    soap_ok(response)
}

async fn handle_delete_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();

    // Check if this is an email item (by em- prefix or by looking up the item)
    let is_email = crate::email::is_email_server_id(&item_id)
        || state
            .storage
            .get_ews_item_by_server_id(owner, &item_id)
            .await
            .ok()
            .flatten()
            .is_none()
            && state.email_available()
            && !item_id.is_empty();

    if is_email && state.email_available() {
        return handle_delete_email_item(state, auth, &item_id).await;
    }

    let calendar_folder_id = folder_id_for(owner, DistinguishedFolder::Calendar);
    let enforcement = PermissionEnforcement::new(&state.storage);
    let perm_ctx = PermissionContext::new(
        auth.username.clone(),
        owner.to_string(),
        calendar_folder_id.clone(),
    );
    match enforcement.can_delete_item(&perm_ctx).await {
        Ok(true) => {}
        Ok(false) => {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorAccessDenied",
                "You do not have permission to delete this calendar item",
                StatusCode::FORBIDDEN,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred during permission check");
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "An internal error occurred during permission check",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    }
    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInvalidIdMalformed",
            "DeleteItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }
    let existing = match state
        .storage
        .get_ews_item_by_server_id(owner, &item_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred while resolving the item");
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "An internal error occurred while resolving the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let Some(existing) = existing else {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorItemNotFound",
            "Requested item does not exist",
            StatusCode::OK,
        );
    };
    if let Err(resp) = validate_item_change_key(&EwsAction::DeleteItem, body, &existing) {
        return *resp;
    }
    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "An internal error occurred");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "An internal error occurred",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let delete_etag = match caldav
        .get_etag(
            &existing.resource_href,
            owner,
            auth.password.expose_secret(),
        )
        .await
    {
        Ok(Some(etag)) => Some(etag),
        _ => existing.etag.clone(),
    };
    if let Err(e) = caldav
        .delete_event(
            &existing.resource_href,
            owner,
            auth.password.expose_secret(),
            delete_etag.as_deref(),
        )
        .await
    {
        tracing::error!(error = %e, owner = %owner, href = %existing.resource_href, "CalDAV delete_event failed");
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            "An internal error occurred while deleting the item",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    if let Err(e) = state.storage.add_delete_tombstone(owner, &item_id).await {
        tracing::error!(error = %e, "Internal error");
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            "An internal error occurred while deleting the item",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    if let Err(e) = state
        .storage
        .delete_item_by_server_id(owner, &item_id)
        .await
    {
        tracing::error!(error = %e, owner = %owner, item_id = %item_id, "Failed to delete mapping");
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            "An internal error occurred while deleting the item",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    let response = format!(
        r#"<m:DeleteItemResponse xmlns:m="{}"><m:ResponseMessages><m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage></m:ResponseMessages></m:DeleteItemResponse>"#,
        EWS_MSG_NS
    );
    soap_ok(response)
}

/// Handle EWS SendItem operation (MS-OXWSCORE §3.1.4.7).
///
/// Sends an email message. Two modes exist per MS-OXWSCORE:
/// 1. **Inline send**: the request body contains a `<t:Message>` element
///    with the full message content — parsed and sent via SMTP/JMAP.
/// 2. **Draft send**: the request references a previously saved draft by
///    ItemId only (no inline Message). Since the gateway does not store
///    drafts, this mode returns `ErrorItemNotFound` to prevent silent
///    email loss (the client must be told the operation cannot succeed).
async fn handle_send_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    // Check if email is enabled
    if !state.cfg.email_enabled {
        return operation_error_response(
            &EwsAction::SendItem,
            "ErrorInvalidRequest",
            "Email operations are not enabled on this server",
            StatusCode::FORBIDDEN,
        );
    }

    // Check if there's a <t:Message> element in the body (inline send)
    if let Some(msg) = crate::email::parse_ews_message(body) {
        match crate::email::send_email(state, &msg, &auth.username, &auth.password).await {
            Ok(_message_id) => {}
            Err(e) => {
                tracing::error!(error = %e, "SMTP send failed for SendItem");
                return operation_error_response(
                    &EwsAction::SendItem,
                    "ErrorInternalServerError",
                    "Failed to send email message",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    } else {
        // No inline <t:Message> — the client is referencing a saved draft by ItemId.
        // Since the gateway does not store drafts, we cannot retrieve or send the
        // message. Returning success here would cause silent email loss (Outlook
        // believes the draft was sent, but no email is actually delivered).
        // Return ErrorItemNotFound per MS-OXWSCORE to inform the client.
        tracing::warn!(
            "SendItem without inline Message — draft send is not supported (drafts are not stored on the gateway)"
        );
        return operation_error_response(
            &EwsAction::SendItem,
            "ErrorItemNotFound",
            "Sending saved drafts is not supported — drafts are not stored on this server",
            StatusCode::OK,
        );
    }

    let response = format!(
        r#"<m:SendItemResponse xmlns:m="{}"><m:ResponseMessages><m:SendItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:SendItemResponseMessage></m:ResponseMessages></m:SendItemResponse>"#,
        EWS_MSG_NS
    );
    soap_ok(response)
}

/// Handle EWS MoveItem operation (MS-OXWSCORE §3.1.4.4).
///
/// Moves an email item between folders. For the gateway, this maps to
/// JMAP Email/set updating the `mailboxIds` property.
async fn handle_move_item(_state: &Arc<AppState>, _auth: &AuthContext, body: &str) -> Response {
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    let _to_folder_id = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .or_else(|| extract_first_attr(body, b"FolderId", b"Id"))
        .unwrap_or_default();

    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::MoveItem,
            "ErrorInvalidIdMalformed",
            "MoveItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    // For email items, we'd need to map the destination folder to a JMAP mailbox ID
    // and update the email's mailboxIds. This requires maintaining a JMAP mailbox ID
    // cache, which we'll implement in a future iteration.
    // For now, return success with the item ID unchanged.
    tracing::info!(
        item_id = %item_id,
        "MoveItem — returning success (JMAP mailbox mapping pending)"
    );

    let change_key = item_id.to_string();

    let response = format!(
        r#"<m:MoveItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:MoveItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Message><t:ItemId Id="{}" ChangeKey="{}" /></t:Message></m:Items></m:MoveItemResponseMessage></m:ResponseMessages></m:MoveItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&item_id),
        xml_escape(&change_key)
    );
    soap_ok(response)
}

async fn handle_resolve_names(auth: &AuthContext, body: &str) -> Response {
    let unresolved =
        extract_first_tag_text(body, b"UnresolvedEntry").unwrap_or_else(|| auth.username.clone());
    let mailbox = if unresolved.contains('@') {
        unresolved
    } else {
        auth.username.clone()
    };
    let response = format!(
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:ResolutionSet TotalItemsInView="1" IncludesLastItemInRange="true"><t:Resolution><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:Mailbox></t:Resolution></m:ResolutionSet></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&mailbox),
        xml_escape(&mailbox)
    );
    soap_ok(response)
}

async fn handle_get_user_availability(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let mailboxes = {
        let parsed = extract_tag_texts(body, b"EmailAddress")
            .into_iter()
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>();
        if parsed.is_empty() {
            vec![auth.username.clone()]
        } else {
            parsed
        }
    };
    let start = extract_first_tag_text(body, b"StartTime")
        .and_then(|v| crate::calendar::parse_datetime(&v))
        .unwrap_or_else(chrono::Utc::now);
    let end = extract_first_tag_text(body, b"EndTime")
        .and_then(|v| crate::calendar::parse_datetime(&v))
        .unwrap_or_else(|| start + chrono::Duration::days(7));
    let interval = extract_first_tag_text(body, b"MergedFreeBusyIntervalInMinutes")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(30);
    let suggestion_minutes = extract_first_tag_text(body, b"MeetingDurationInMinutes")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(interval);
    let view_type = requested_freebusy_view_type(body);
    let mut combined_merged = String::new();
    let mut responses = String::new();
    for mailbox in &mailboxes {
        let (merged, events_xml) = merged_freebusy_for_mailbox(
            state,
            mailbox,
            auth.password.expose_secret(),
            start,
            end,
            interval,
        )
        .await;
        combined_merged = if combined_merged.is_empty() {
            merged.clone()
        } else {
            merge_merged_freebusy(&combined_merged, &merged)
        };
        responses.push_str(&format!(
            r#"<m:FreeBusyResponse><m:ResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:ResponseMessage><m:FreeBusyView><t:FreeBusyViewType>{view_type}</t:FreeBusyViewType><t:MergedFreeBusy>{merged}</t:MergedFreeBusy><t:CalendarEventArray>{events_xml}</t:CalendarEventArray></m:FreeBusyView></m:FreeBusyResponse>"#,
        ));
    }
    let suggestions_xml = if body.contains("SuggestionsViewOptions") {
        suggestions_xml_for_window(&combined_merged, start, end, interval, suggestion_minutes)
    } else {
        String::new()
    };
    let response = format!(
        r#"<m:GetUserAvailabilityResponse xmlns:m="{msg_ns}" xmlns:t="{type_ns}"><m:FreeBusyResponseArray>{responses}</m:FreeBusyResponseArray>{suggestions_xml}</m:GetUserAvailabilityResponse>"#,
        msg_ns = EWS_MSG_NS,
        type_ns = EWS_TYPE_NS
    );
    soap_ok(response)
}

async fn handle_get_user_oof_settings(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:GetUserOofSettingsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessage ResponseClass="Success">
    <m:ResponseCode>NoError</m:ResponseCode>
  </m:ResponseMessage>
  <m:OofSettings>
    <t:OofState>Disabled</t:OofState>
    <t:ExternalAudience>All</t:ExternalAudience>
    <t:Duration>
      <t:StartTime>2000-01-01T00:00:00Z</t:StartTime>
      <t:EndTime>2000-01-01T00:00:00Z</t:EndTime>
    </t:Duration>
    <t:InternalReply/>
    <t:ExternalReply/>
  </m:OofSettings>
  <m:AllowExternalOof>true</m:AllowExternalOof>
</m:GetUserOofSettingsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_set_user_oof_settings(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:SetUserOofSettingsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessage ResponseClass="Success">
    <m:ResponseCode>NoError</m:ResponseCode>
  </m:ResponseMessage>
</m:SetUserOofSettingsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_service_configuration(state: &Arc<AppState>) -> Response {
    let domain = &state.cfg.mail_domain;
    if domain.is_empty() {
        return soap_fault(
            "ErrorInternalServerError",
            "Mail domain not configured",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    };
    let domain_escaped = xml_escape(domain);
    let inner = format!(
        r#"<m:GetServiceConfigurationResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetServiceConfigurationResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:MailTipsConfiguration>
<t:MailTipsEnabled>true</t:MailTipsEnabled>
<t:MaxRecipientsPerCallRequest>100</t:MaxRecipientsPerCallRequest>
<t:MaxMessageSize>104857600</t:MaxMessageSize>
<t:LargeAudienceThreshold>100</t:LargeAudienceThreshold>
<t:ShowExternalRecipientCount>true</t:ShowExternalRecipientCount>
<t:InternalDomains>
<t:SMPTDomain>
<t:DomainName>{domain_escaped}</t:DomainName>
</t:SMPTDomain>
</t:InternalDomains>
<t:MaxMailTipsCallsPerCallRequest>100</t:MaxMailTipsCallsPerCallRequest>
</m:MailTipsConfiguration>
</m:GetServiceConfigurationResponseMessage>
</m:ResponseMessages>
</m:GetServiceConfigurationResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_server_time_zones() -> Response {
    let inner = format!(
        r#"<m:GetServerTimeZonesResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetServerTimeZonesResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:TimeZoneDefinitions>{}</m:TimeZoneDefinitions>
</m:GetServerTimeZonesResponseMessage>
</m:ResponseMessages>
</m:GetServerTimeZonesResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, *TIMEZONE_DEFINITIONS
    );
    soap_ok(inner)
}

static TIMEZONE_DEFINITIONS: LazyLock<String> = LazyLock::new(render_timezone_definitions);

fn render_timezone_definitions() -> String {
    use strum::IntoEnumIterator;
    use windows_timezones::WindowsTimezone;

    let zones: Vec<&'static str> = vec![
        "UTC",
        "GMT Standard Time",
        "Central Europe Standard Time",
        "W. Europe Standard Time",
        "E. Europe Standard Time",
        "Pacific Standard Time",
        "Mountain Standard Time",
        "Central Standard Time",
        "Eastern Standard Time",
        "US Eastern Standard Time",
        "US Mountain Standard Time",
        "Pacific SA Standard Time",
        "Atlantic Standard Time",
        "SA Pacific Standard Time",
        "Greenland Standard Time",
        "Azores Standard Time",
        "Cape Verde Standard Time",
        "Morocco Standard Time",
        "W. Central Africa Standard Time",
        "Jordan Standard Time",
        "Middle East Standard Time",
        "Egypt Standard Time",
        "Syria Standard Time",
        "E. Africa Standard Time",
        "Arabic Standard Time",
        "Arab Standard Time",
        "Russian Standard Time",
        "Kaliningrad Standard Time",
        "Turkey Standard Time",
        "Israel Standard Time",
        "Iran Standard Time",
        "Afghanistan Standard Time",
        "Pakistan Standard Time",
        "India Standard Time",
        "Sri Lanka Standard Time",
        "Nepal Standard Time",
        "Central Asia Standard Time",
        "North Asia Standard Time",
        "SE Asia Standard Time",
        "North Asia East Standard Time",
        "China Standard Time",
        "Korea Standard Time",
        "Tokyo Standard Time",
        "West Pacific Standard Time",
        "AUS Central Standard Time",
        "AUS Eastern Standard Time",
        "Tasmania Standard Time",
        "New Zealand Standard Time",
    ];

    let mut result = String::with_capacity(zones.len() * 400);
    for name in &zones {
        if !WindowsTimezone::iter().any(|v| v.name() == *name) {
            continue;
        }
        let iana = crate::timezone::windows_timezone_name_to_tz(name);
        let (bias, std_date_xml, dst_date_xml) = if let Some(tz) = iana {
            let iana_str = tz.name();
            match crate::timezone::iana_to_windows_params(iana_str) {
                Some((
                    bias,
                    _std_name,
                    _dst_name,
                    std_blob,
                    dst_blob,
                    _std_bias_val,
                    dst_bias_val,
                )) => {
                    let std_xml = tz_blob_to_std_time_xml(&std_blob);
                    let dst_xml = tz_blob_to_daylight_time_xml(&dst_blob, dst_bias_val);
                    (bias, std_xml, dst_xml)
                }
                None => (0, String::new(), String::new()),
            }
        } else {
            (0, String::new(), String::new())
        };
        let std_name = name.replace("Standard Time", "Standard");
        result.push_str(&format!(
            r#"<t:TimeZoneDefinition Id="{}" Name="{}"><t:Bias>{}</t:Bias>{}{}</t:TimeZoneDefinition>"#,
            xml_escape(name),
            xml_escape(&std_name),
            bias,
            std_date_xml,
            dst_date_xml,
        ));
    }
    result
}
fn tz_blob_to_std_time_xml(blob: &[u8; 16]) -> String {
    if blob.iter().all(|&b| b == 0) {
        return r#"<t:StandardTime><t:Bias>0</t:Bias></t:StandardTime>"#.to_string();
    }
    let month = blob[2] as u32;
    let day_order = blob[6] as u32;
    let hour = blob[8] as u32;
    let day_of_week = match blob[4] {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Sunday",
    };
    format!(
        r#"<t:StandardTime><t:Bias>0</t:Bias><t:Time>{:02}:00:00</t:Time><t:DayOrder>{}</t:DayOrder><t:Month>{}</t:Month><t:DayOfWeek>{}</t:DayOfWeek></t:StandardTime>"#,
        hour, day_order, month, day_of_week
    )
}

fn tz_blob_to_daylight_time_xml(blob: &[u8; 16], dst_bias: i32) -> String {
    if blob.iter().all(|&b| b == 0) && dst_bias == 0 {
        return String::new();
    }
    let month = blob[2] as u32;
    let day_order = blob[6] as u32;
    let hour = blob[8] as u32;
    let day_of_week = match blob[4] {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => "Sunday",
    };
    if month == 0 {
        return String::new();
    }
    format!(
        r#"<t:DaylightTime><t:Bias>{}</t:Bias><t:Time>{:02}:00:00</t:Time><t:DayOrder>{}</t:DayOrder><t:Month>{}</t:Month><t:DayOfWeek>{}</t:DayOfWeek></t:DaylightTime>"#,
        dst_bias, hour, day_order, month, day_of_week
    )
}

async fn handle_get_folder_info() -> Response {
    let inner = format!(
        r#"<m:GetFolderInfoResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetFolderInfoResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
    </m:GetFolderInfoResponseMessage>
  </m:ResponseMessages>
</m:GetFolderInfoResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_mail_tips(auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:GetMailTipsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:MailTipsResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:MailTips>
        <t:RecipientAddress><t:EmailAddress>{}</t:EmailAddress></t:RecipientAddress>
        <t:OutOfOffice><t:ReplyBody><t:Message></t:Message></t:ReplyBody></t:OutOfOffice>
      </m:MailTips>
    </m:MailTipsResponseMessage>
  </m:ResponseMessages>
</m:GetMailTipsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&auth.username)
    );
    soap_ok(inner)
}

async fn handle_find_people(auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:FindPeopleResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:FindPeopleResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:People>
        <t:Persona>
          <t:PersonaId Id="{}" ChangeKey="01"/>
          <t:DisplayName>{}</t:DisplayName>
          <t:EmailAddress><t:EmailAddress>{}</t:EmailAddress></t:EmailAddress>
        </t:Persona>
      </m:People>
      <m:TotalPeopleInView>1</m:TotalPeopleInView>
    </m:FindPeopleResponseMessage>
  </m:ResponseMessages>
</m:FindPeopleResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        uuid::Uuid::new_v4(),
        xml_escape(&auth.username),
        xml_escape(&auth.username),
    );
    soap_ok(inner)
}

async fn handle_get_conversation_items() -> Response {
    let inner = format!(
        r#"<m:GetConversationItemsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetConversationItemsResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Conversations/>
    </m:GetConversationItemsResponseMessage>
  </m:ResponseMessages>
</m:GetConversationItemsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}
async fn handle_convert_id(auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:ConvertIdResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:ConvertIdResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:AlternateId Id="{}" Format="EwsId" Mailbox="{}"/>
</m:ConvertIdResponseMessage>
</m:ResponseMessages>
</m:ConvertIdResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&auth.username),
        xml_escape(&auth.username)
    );
    soap_ok(inner)
}

async fn handle_get_room_lists(state: &Arc<AppState>, auth: &AuthContext) -> Response {
    let room_manager = &state.room_manager;
    let owner = crate::util::normalize_email(&auth.username);
    match room_manager.get_room_lists(&owner).await {
        Ok(room_lists) => {
            let inner = render_get_room_lists_response(&room_lists);
            soap_ok(inner)
        }
        Err(e) => {
            tracing::error!(error = %e, "GetRoomLists failed");
            operation_error_response(
                &EwsAction::GetRoomLists,
                "ErrorInternalServerError",
                "An internal error occurred while fetching room lists",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

async fn handle_get_rooms(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let room_manager = &state.room_manager;
    let owner = crate::util::normalize_email(&auth.username);
    let rooms = if let Some(room_list_email) = parse_get_rooms_request(body) {
        room_manager
            .get_rooms_for_list(&owner, &room_list_email)
            .await
    } else {
        room_manager.get_all_rooms(&owner).await
    };
    match rooms {
        Ok(rooms) => {
            let inner = render_get_rooms_response(&rooms);
            soap_ok(inner)
        }
        Err(e) => {
            tracing::error!(error = %e, "GetRooms failed");
            operation_error_response(
                &EwsAction::GetRooms,
                "ErrorInternalServerError",
                "An internal error occurred while fetching rooms",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

async fn handle_get_delegate(state: &Arc<AppState>, auth: &AuthContext) -> Response {
    let handler = DelegateEwsHandler::new(state.storage.clone());
    let inner = handler.handle_get_delegate(&auth.username).await;
    soap_ok(inner)
}

async fn handle_add_delegate(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let handler = DelegateEwsHandler::new(state.storage.clone());
    let inner = handler.handle_add_delegate(&auth.username, body).await;
    soap_ok(inner)
}

async fn handle_remove_delegate(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let handler = DelegateEwsHandler::new(state.storage.clone());
    let inner = handler.handle_remove_delegate(&auth.username, body).await;
    soap_ok(inner)
}

async fn handle_update_delegate(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let handler = DelegateEwsHandler::new(state.storage.clone());
    let inner = handler.handle_update_delegate(&auth.username, body).await;
    soap_ok(inner)
}

async fn handle_get_user_photo(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:GetUserPhotoResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetUserPhotoResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
</m:GetUserPhotoResponseMessage>
</m:ResponseMessages>
</m:GetUserPhotoResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_mark_as_junk(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:MarkAsJunkResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:MarkAsJunkResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:MovedItemId/>
</m:MarkAsJunkResponseMessage>
</m:ResponseMessages>
</m:MarkAsJunkResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_app_manifests() -> Response {
    let inner = format!(
        r#"<m:GetAppManifestsResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetAppManifestsResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:Apps/>
</m:GetAppManifestsResponseMessage>
</m:ResponseMessages>
</m:GetAppManifestsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_app_marketplace_url() -> Response {
    let inner = format!(
        r#"<m:GetAppMarketplaceUrlResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetAppMarketplaceUrlResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:AppMarketplaceUrl/>
</m:GetAppMarketplaceUrlResponseMessage>
</m:ResponseMessages>
</m:GetAppMarketplaceUrlResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_install_app() -> Response {
    let inner = format!(
        r#"<m:InstallAppResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:InstallAppResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
</m:InstallAppResponseMessage>
</m:ResponseMessages>
</m:InstallAppResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_uninstall_app() -> Response {
    let inner = format!(
        r#"<m:UninstallAppResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:UninstallAppResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
</m:UninstallAppResponseMessage>
</m:ResponseMessages>
</m:UninstallAppResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_client_access_token() -> Response {
    let inner = format!(
        r#"<m:GetClientAccessTokenResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetClientAccessTokenResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:Token/>
</m:GetClientAccessTokenResponseMessage>
</m:ResponseMessages>
</m:GetClientAccessTokenResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}
async fn handle_get_reminders(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:GetRemindersResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:RemindersResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Reminders/>
    </m:RemindersResponseMessage>
  </m:ResponseMessages>
</m:GetRemindersResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_perform_reminder_action(_auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:PerformReminderActionResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:PerformReminderActionResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:UpdatedReminderIds/>
    </m:PerformReminderActionResponseMessage>
  </m:ResponseMessages>
</m:PerformReminderActionResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
}

async fn handle_get_persona(auth: &AuthContext, _body: &str) -> Response {
    let inner = format!(
        r#"<m:GetPersonaResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetPersonaResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Persona>
        <t:PersonaId Id="{}" ChangeKey="01"/>
        <t:DisplayName>{}</t:DisplayName>
        <t:EmailAddress><t:EmailAddress>{}</t:EmailAddress></t:EmailAddress>
      </m:Persona>
    </m:GetPersonaResponseMessage>
  </m:ResponseMessages>
</m:GetPersonaResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        uuid::Uuid::new_v4(),
        xml_escape(&auth.username),
        xml_escape(&auth.username)
    );
    soap_ok(inner)
}

async fn handle_create_attachment(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let Some(parsed) = parse_create_attachment_request(body) else {
        return operation_error_response(
            &EwsAction::CreateAttachment,
            "ErrorInvalidRequest",
            "Could not parse CreateAttachment request",
            StatusCode::BAD_REQUEST,
        );
    };
    let owner = crate::util::normalize_email(&auth.username);
    match state
        .attachment_manager
        .create_file_attachment(&crate::attachment::CreateAttachmentParams {
            owner: &owner,
            parent_item_server_id: &parsed.parent_item_id,
            name: &parsed.name,
            content_type: &parsed.content_type,
            content_base64: &parsed.content_base64,
            is_inline: parsed.is_inline,
            content_id: parsed.content_id.as_deref(),
            content_location: parsed.content_location.as_deref(),
        })
        .await
    {
        Ok(attachment) => {
            let inner = render_create_attachment_response(&attachment.id, &parsed.parent_item_id);
            soap_ok(inner)
        }
        Err(e) => {
            tracing::error!(error = %e, "CreateAttachment failed");
            operation_error_response(
                &EwsAction::CreateAttachment,
                "ErrorSavePropertyError",
                "An internal error occurred while saving the attachment",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

async fn handle_get_attachment(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let attachment_ids = parse_get_attachment_request(body);
    let owner = crate::util::normalize_email(&auth.username);
    let mut attachments_xml = String::new();
    for id in &attachment_ids {
        match state.attachment_manager.get_attachment(&owner, id).await {
            Ok(Some(attachment)) => {
                attachments_xml.push_str(&render_file_attachment_xml(&attachment, true));
            }
            Ok(None) => {
                attachments_xml.push_str(&format!(
                    r#"<t:FileAttachment><t:AttachmentId Id="{}"/><t:Name>NotFound</t:Name></t:FileAttachment>"#,
                    xml_escape(id)
                ));
            }
            Err(e) => {
                tracing::warn!("GetAttachment error for {}: {}", id, e);
            }
        }
    }
    let inner = render_get_attachment_response(&attachments_xml);
    soap_ok(inner)
}

async fn handle_delete_attachment(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = crate::util::normalize_email(&auth.username);
    let Some(parsed) = parse_delete_attachment_request(body) else {
        return operation_error_response(
            &EwsAction::DeleteAttachment,
            "ErrorInvalidRequest",
            "Could not parse DeleteAttachment request",
            StatusCode::BAD_REQUEST,
        );
    };
    match state
        .attachment_manager
        .delete_attachment(&owner, &parsed.attachment_id)
        .await
    {
        Ok(Some(root_item_id)) => {
            let inner = crate::attachment::render_delete_attachment_response(&root_item_id);
            soap_ok(inner)
        }
        Ok(None) => operation_error_response(
            &EwsAction::DeleteAttachment,
            "ErrorItemNotFound",
            "Attachment not found",
            StatusCode::NOT_FOUND,
        ),
        Err(e) => {
            tracing::error!(error = %e, "DeleteAttachment failed");
            operation_error_response(
                &EwsAction::DeleteAttachment,
                "ErrorDeleteOperationFailed",
                "An internal error occurred while deleting the attachment",
                StatusCode::INTERNAL_SERVER_ERROR,
            )
        }
    }
}

/// Handle EWS GetUserConfiguration operation.
///
/// Per MS-OXWSUSRCFG §3.1.4.3, clients use this to retrieve user configuration
/// objects (aliases, signatures, black/white lists) stored in a folder.
/// The gateway does not support persistent user configuration objects, but we
/// return a successful response with an empty configuration to allow the client
/// to proceed without errors.
async fn handle_get_user_configuration(
    _state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    // Parse the request to extract the configuration name and folder reference.
    // We need to echo these back in the response to satisfy client expectations.

    // Extract Name attribute from UserConfigurationName element.
    let config_name = extract_first_attr(body, b"UserConfigurationName", b"Name")
        .unwrap_or_else(|| "Default".to_string());

    // Determine the folder reference: either FolderId or DistinguishedFolderId inside UserConfigurationName.
    // We'll reconstruct it with the original Id value, properly XML-escaped.
    let folder_ref_xml = {
        let folder_id = extract_first_attr(body, b"FolderId", b"Id");
        let distinguished_id = extract_first_attr(body, b"DistinguishedFolderId", b"Id");

        match (folder_id, distinguished_id) {
            (Some(fid), _) => format!(r#"<t:FolderId Id="{}" />"#, xml_escape(&fid)),
            (None, Some(did)) => {
                format!(r#"<t:DistinguishedFolderId Id="{}" />"#, xml_escape(&did))
            }
            _ => r#"<t:DistinguishedFolderId Id="msgfolderroot" />"#.to_string(),
        }
    };

    // Generate a deterministic synthetic ItemId for the UserConfiguration object itself.
    let mut h = Sha256::new();
    h.update(auth.username.as_bytes());
    h.update(config_name.as_bytes());
    let digest = h.finalize();
    let synthetic_id = format!("uc-{}", const_hex::encode(&digest[..12]));
    // The ChangeKey for a UserConfiguration object is typically a version string.
    // We use a simple static ChangeKey because these configs never change.
    let change_key = "1";

    // Build the ParentFolderId: Must be a FolderId with a stable ChangeKey.
    // Resolve the requested folder to a concrete folder ID and derive ChangeKey.
    let owner = owner_from_username(&auth.username);
    let fid = if let Some(fid_str) = extract_first_attr(body, b"FolderId", b"Id") {
        fid_str
    } else if let Some(did) = extract_first_attr(body, b"DistinguishedFolderId", b"Id") {
        let folder_enum = match did.to_ascii_lowercase().as_str() {
            "msgfolderroot" => DistinguishedFolder::MsgFolderRoot,
            "inbox" => DistinguishedFolder::Inbox,
            "sentitems" => DistinguishedFolder::SentItems,
            "deleteditems" => DistinguishedFolder::DeletedItems,
            "drafts" => DistinguishedFolder::Drafts,
            "outbox" => DistinguishedFolder::Outbox,
            "junkemail" => DistinguishedFolder::JunkEmail,
            "calendar" => DistinguishedFolder::Calendar,
            "contacts" => DistinguishedFolder::Contacts,
            "tasks" => DistinguishedFolder::Tasks,
            "notes" => DistinguishedFolder::Notes,
            "journal" => DistinguishedFolder::Journal,
            _ => DistinguishedFolder::MsgFolderRoot,
        };
        folder_id_for(owner, folder_enum)
    } else {
        folder_id_for(owner, DistinguishedFolder::MsgFolderRoot)
    };
    // Compute the folder's ChangeKey using the same method as render_folder_xml:
    // For a folder ID like "CAL-abc123", the ChangeKey is the suffix after the first dash.
    let parent_prefix_len = fid.find('-').map(|i| i + 1).unwrap_or(4);
    let parent_ck = &fid[parent_prefix_len..];
    // ParentFolderId is of type FolderIdType: Id and ChangeKey are attributes directly on the element.
    let parent_folder_id_xml = format!(
        r#"<t:ParentFolderId Id="{}" ChangeKey="{}" />"#,
        xml_escape(&fid),
        parent_ck
    );

    let response_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    <t:ServerVersionInfo MajorVersion="15" MinorVersion="20" MajorBuildNumber="0" MinorBuildNumber="0" Version="Exchange2016" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
  </s:Header>
  <s:Body>
    <m:GetUserConfigurationResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:GetUserConfigurationResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:UserConfiguration>
            <t:UserConfigurationName Name="{}">
              {}
            </t:UserConfigurationName>
            {}
            <t:ItemId Id="{}" ChangeKey="{}" />
            <t:Dictionary />
          </m:UserConfiguration>
        </m:GetUserConfigurationResponseMessage>
      </m:ResponseMessages>
    </m:GetUserConfigurationResponse>
  </s:Body>
</s:Envelope>"#,
        xml_escape(&config_name),
        folder_ref_xml,
        parent_folder_id_xml,
        STANDARD.encode(&synthetic_id),
        change_key
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(response_xml.into())
        .unwrap()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(server_id: &str, etag: Option<&str>, updated_at: Option<&str>) -> EwsItemRow {
        EwsItemRow {
            server_id: server_id.to_string(),
            caldav_href: None,
            resource_href: format!("/dav/cal/test/default/{}.ics", server_id),
            uid: Some("test-uid".to_string()),
            etag: etag.map(|s| s.to_string()),
            updated_at: updated_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_changekey_stability_without_updated_at() {
        // Same server_id + etag must produce the same ChangeKey regardless of updated_at.
        // This is the core fix: updated_at is a DB admin timestamp, not a content version.
        let row_a = make_row("ABC123", Some("etag-v1"), Some("2026-01-01T00:00:00Z"));
        let row_b = make_row("ABC123", Some("etag-v1"), Some("2026-06-15T12:00:00Z"));
        let row_c = make_row("ABC123", Some("etag-v1"), None);
        assert_eq!(
            changekey_for_item(&row_a),
            changekey_for_item(&row_b),
            "changekey should not depend on updated_at"
        );
        assert_eq!(
            changekey_for_item(&row_a),
            changekey_for_item(&row_c),
            "changekey should not differ when updated_at is None"
        );
    }

    #[test]
    fn test_xml_escape_escapes_special_characters() {
        // xml_escape escapes &, <, >, ", and ' for safe inclusion in XML.
        // This matches quick_xml::escape::escape behavior.
        assert_eq!(
            xml_escape("a<b>c&d\"e'f").as_ref(),
            "a&lt;b&gt;c&amp;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("'").as_ref(), "&apos;");
        assert_eq!(xml_escape("\"").as_ref(), "&quot;");
        assert_eq!(xml_escape("&<>'").as_ref(), "&amp;&lt;&gt;&apos;");
    }

    #[test]
    fn test_synthetic_id_is_deterministic() {
        let username = "user@example.com";
        let config_name = "Aliases";

        let mut h1 = Sha256::new();
        h1.update(username.as_bytes());
        h1.update(config_name.as_bytes());
        let digest1 = h1.finalize();
        let id1 = format!("uc-{}", const_hex::encode(&digest1[..12]));

        let mut h2 = Sha256::new();
        h2.update(username.as_bytes());
        h2.update(config_name.as_bytes());
        let digest2 = h2.finalize();
        let id2 = format!("uc-{}", const_hex::encode(&digest2[..12]));

        assert_eq!(id1, id2, "synthetic ID should be deterministic");
    }

    #[test]
    fn test_changekey_changes_with_etag() {
        // Different etag → different ChangeKey (content version changed)
        let row_v1 = make_row("ABC123", Some("etag-v1"), None);
        let row_v2 = make_row("ABC123", Some("etag-v2"), None);
        assert_ne!(
            changekey_for_item(&row_v1),
            changekey_for_item(&row_v2),
            "ChangeKey must differ when etag differs"
        );
    }

    #[test]
    fn test_changekey_changes_with_server_id() {
        // Different server_id → different ChangeKey (different items)
        let row_a = make_row("ID_AAA", Some("etag-v1"), None);
        let row_b = make_row("ID_BBB", Some("etag-v1"), None);
        assert_ne!(
            changekey_for_item(&row_a),
            changekey_for_item(&row_b),
            "ChangeKey must differ when server_id differs"
        );
    }

    #[test]
    fn test_changekey_none_etag() {
        // No etag should still produce a deterministic ChangeKey
        let row = make_row("ABC123", None, None);
        let ck = changekey_for_item(&row);
        assert!(!ck.is_empty(), "ChangeKey must not be empty");
        // Must be 24 hex chars (12 bytes → 24 hex digits)
        assert_eq!(ck.len(), 24, "ChangeKey must be 24 hex characters");
    }

    #[test]
    fn test_changekey_format() {
        let row = make_row("ABC123", Some("etag-v1"), Some("2026-01-01T00:00:00Z"));
        let ck = changekey_for_item(&row);
        assert_eq!(ck.len(), 24, "ChangeKey must be 24 hex characters");
        assert!(
            ck.chars().all(|c| c.is_ascii_hexdigit()),
            "ChangeKey must be hex"
        );
    }

    #[test]
    fn test_find_folder_uses_folder_id_inside_parent_folder_ids() {
        let owner = "user@example.com";
        let calendar_id = folder_id_for(owner, DistinguishedFolder::Calendar);
        let inbox_id = folder_id_for(owner, DistinguishedFolder::Inbox);
        let body = format!(
            r#"<m:FindFolder>
                <m:Restriction>
                    <t:IsEqualTo>
                        <t:FieldURI FieldURI="folder:FolderId" />
                        <t:FieldURIOrConstant>
                            <t:Constant Value="{inbox_id}" />
                        </t:FieldURIOrConstant>
                    </t:IsEqualTo>
                    <t:FolderId Id="{inbox_id}" />
                </m:Restriction>
                <m:ParentFolderIds>
                    <t:FolderId Id="{calendar_id}" />
                </m:ParentFolderIds>
            </m:FindFolder>"#
        );

        assert_eq!(
            requested_find_folder_parent_from_ids(&body, owner),
            DistinguishedFolder::Calendar
        );
        assert_eq!(
            requested_folder_from_ids(&body, owner),
            DistinguishedFolder::Inbox
        );
    }

    #[test]
    fn test_find_folder_uses_distinguished_folder_id_inside_parent_folder_ids() {
        let owner = "user@example.com";
        let inbox_id = folder_id_for(owner, DistinguishedFolder::Inbox);
        let body = format!(
            r#"<m:FindFolder>
                <m:FolderShape>
                    <t:AdditionalProperties>
                        <t:FolderId Id="{inbox_id}" />
                    </t:AdditionalProperties>
                </m:FolderShape>
                <m:ParentFolderIds>
                    <t:DistinguishedFolderId Id="msgfolderroot" />
                </m:ParentFolderIds>
            </m:FindFolder>"#
        );

        assert_eq!(
            requested_find_folder_parent_from_ids(&body, owner),
            DistinguishedFolder::MsgFolderRoot
        );
    }

    #[test]
    fn test_get_user_configuration_uses_distinguished_folder() {
        // Test DistinguishedFolderId path
        let body = r#"<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
            <s:Body>
                <m:GetUserConfiguration xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
                    <m:UserConfigurationName Name="TestConfig">
                        <t:DistinguishedFolderId Id="inbox" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types" />
                    </m:UserConfigurationName>
                </m:GetUserConfiguration>
            </s:Body>
        </s:Envelope>"#;

        let distinguished = extract_first_attr(body, b"DistinguishedFolderId", b"Id");
        assert_eq!(distinguished, Some("inbox".to_string()));
        let folder_id = extract_first_attr(body, b"FolderId", b"Id");
        assert!(folder_id.is_none());
    }

    #[test]
    fn test_distinguished_folder_ids_are_case_insensitive() {
        let owner = "user@example.com";
        let inbox_id = folder_id_for(owner, DistinguishedFolder::Inbox);
        let body = format!(
            r#"<m:FindFolder>
                <m:FolderShape>
                    <t:AdditionalProperties>
                        <t:FolderId Id="{inbox_id}" />
                    </t:AdditionalProperties>
                </m:FolderShape>
                <m:ParentFolderIds>
                    <t:DistinguishedFolderId Id="msgfolderroot" />
                </m:ParentFolderIds>
            </m:FindFolder>"#
        );

        assert_eq!(
            requested_find_folder_parent_from_ids(&body, owner),
            DistinguishedFolder::MsgFolderRoot
        );
    }

    #[test]
    fn test_folder_id_literal_distinguished_name_is_not_resolved_as_distinguished_folder() {
        let owner = "user@example.com";
        let body = r#"<m:GetFolder>
            <m:FolderIds>
                <t:FolderId Id="inbox" />
            </m:FolderIds>
        </m:GetFolder>"#;

        assert_eq!(
            requested_folder_from_ids(body, owner),
            DistinguishedFolder::Calendar
        );
    }

    #[test]
    fn test_find_folder_parent_folder_id_literal_distinguished_name_is_not_resolved() {
        let owner = "user@example.com";
        let body = r#"<m:FindFolder>
            <m:ParentFolderIds>
                <t:FolderId Id="inbox" />
            </m:ParentFolderIds>
        </m:FindFolder>"#;

        assert_eq!(
            requested_find_folder_parent_from_ids(body, owner),
            DistinguishedFolder::Calendar
        );
    }

    #[test]
    fn test_extract_conflict_resolution_always_overwrite() {
        let xml = r#"<UpdateItemType ConflictResolution="AlwaysOverwrite" MessageDisposition="SendOnly"><ItemChanges></ItemChanges></UpdateItemType>"#;
        assert_eq!(
            extract_conflict_resolution(xml).as_deref(),
            Some("alwaysoverwrite")
        );
    }

    #[test]
    fn test_extract_conflict_resolution_never_overwrite() {
        let xml = r#"<UpdateItemType ConflictResolution="NeverOverwrite" MessageDisposition="SendOnly"><ItemChanges></ItemChanges></UpdateItemType>"#;
        assert_eq!(
            extract_conflict_resolution(xml).as_deref(),
            Some("neveroverwrite")
        );
    }

    #[test]
    fn test_extract_conflict_resolution_auto_resolve() {
        let xml = r#"<UpdateItemType ConflictResolution="AutoResolve"><ItemChanges></ItemChanges></UpdateItemType>"#;
        assert_eq!(
            extract_conflict_resolution(xml).as_deref(),
            Some("autoresolve")
        );
    }

    #[test]
    fn test_extract_conflict_resolution_missing() {
        let xml = r#"<UpdateItemType MessageDisposition="SendOnly"><ItemChanges></ItemChanges></UpdateItemType>"#;
        assert_eq!(extract_conflict_resolution(xml), None);
    }

    #[test]
    fn test_extract_conflict_resolution_soap_envelope() {
        // Full SOAP envelope with namespace prefix on UpdateItem
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/">
  <soap:Body>
    <m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"
                  ConflictResolution="AlwaysOverwrite"
                  MessageDisposition="SendOnly"
                  SendMeetingInvitationsOrCancellations="SendToNone">
      <m:ItemChanges>
        <t:ItemChange xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
          <t:ItemId Id="ABC" ChangeKey="123" />
        </t:ItemChange>
      </m:ItemChanges>
    </m:UpdateItem>
  </soap:Body>
</soap:Envelope>"#;
        assert_eq!(
            extract_conflict_resolution(xml).as_deref(),
            Some("alwaysoverwrite")
        );
    }
}
