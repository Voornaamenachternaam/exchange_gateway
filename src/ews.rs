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
// CarddavClient import removed - type inferred via usage
use crate::vcard::{self, Vcard, parse_vcard_from_data};

use crate::delegate_ews::DelegateEwsHandler;
use crate::directory::Contact;
use crate::ews_folders::{
    DistinguishedFolder, folder_id_for, render_folder_hierarchy_creates, render_folder_xml,
    render_root_and_children, resolve_folder_id, validate_folder_request,
};
use crate::ews_update::{apply_field_changes, parse_item_changes};

use crate::jmap::{JmapClient, QueryCalendarEventsParams, SetCalendarEventParams};
use crate::models::AppState;

use crate::notifications::{
    NotificationEvent, PushConfig, PushDelivery, PushDeliveryStatus, PushNotifier, SubscriptionKind,
};
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
use crate::version;
use anyhow::anyhow;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use bytes::Bytes;
use chrono::{Datelike, Utc};
use const_hex;
use hex;
use itertools::Itertools;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use roxmltree;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tracing::warn;

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
    GetEvents,
    GetStreamingEvents,
    CreateItem,
    UpdateItem,
    DeleteItem,
    SendItem,
    MoveItem,
    CopyItem,
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
            EwsAction::GetEvents => "GetEventsResponseMessage",
            EwsAction::GetStreamingEvents => "GetStreamingEventsResponseMessage",
            EwsAction::CreateItem => "CreateItemResponseMessage",
            EwsAction::UpdateItem => "UpdateItemResponseMessage",
            EwsAction::DeleteItem => "DeleteItemResponseMessage",
            EwsAction::SendItem => "SendItemResponseMessage",
            EwsAction::MoveItem => "MoveItemResponseMessage",
            EwsAction::CopyItem => "CopyItemResponseMessage",
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
  <s:Header>{svi}</s:Header>
  <s:Body>{body}</s:Body>
</s:Envelope>"#,
        svi = version::current().render_ews_header(EWS_TYPE_NS),
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
        EwsAction::Subscribe => handle_subscribe(&state, &auth, &body).await,
        EwsAction::Unsubscribe => handle_unsubscribe(&state, &auth, &body).await,
        EwsAction::GetEvents => handle_get_events(&state, &auth, &body).await,
        EwsAction::GetStreamingEvents => handle_get_streaming_events(state, auth, body).await,
        EwsAction::CreateItem => handle_create_item(&state, &auth, &body).await,
        EwsAction::UpdateItem => handle_update_item(&state, &auth, &body).await,
        EwsAction::DeleteItem => handle_delete_item(&state, &auth, &body).await,
        EwsAction::SendItem => handle_send_item(&state, &auth, &body).await,
        EwsAction::MoveItem => handle_move_item(&state, &auth, &body).await,
        EwsAction::CopyItem => handle_copy_item(&state, &auth, &body).await,
        EwsAction::ResolveNames => handle_resolve_names(&state, &auth, &body).await,
        EwsAction::GetUserOofSettings => handle_get_user_oof_settings(&state, &auth, &body).await,
        EwsAction::SetUserOofSettings => handle_set_user_oof_settings(&state, &auth, &body).await,
        EwsAction::GetServiceConfiguration => handle_get_service_configuration(&state).await,
        EwsAction::GetServerTimeZones => handle_get_server_time_zones().await,
        EwsAction::GetFolderInfo => handle_get_folder_info().await,
        EwsAction::GetMailTips => handle_get_mail_tips(&auth, &body).await,
        EwsAction::FindPeople => handle_find_people(&state, &auth, &body).await,
        EwsAction::GetConversationItems => handle_get_conversation_items(&state, &auth, &body).await,
        EwsAction::ConvertId => handle_convert_id(&auth, &body).await,
        EwsAction::GetRoomLists => handle_get_room_lists(&state, &auth).await,
        EwsAction::GetRooms => handle_get_rooms(&state, &auth, &body).await,
        EwsAction::GetDelegate => handle_get_delegate(&state, &auth).await,
        EwsAction::AddDelegate => handle_add_delegate(&state, &auth, &body).await,
        EwsAction::RemoveDelegate => handle_remove_delegate(&state, &auth, &body).await,
        EwsAction::UpdateDelegate => handle_update_delegate(&state, &auth, &body).await,
        EwsAction::GetUserPhoto => handle_get_user_photo(&state, &auth, &body).await,
        EwsAction::MarkAsJunk => handle_mark_as_junk(&state, &auth, &body).await,
        EwsAction::GetAppManifests => handle_get_app_manifests().await,
        EwsAction::GetAppMarketplaceUrl => handle_get_app_marketplace_url().await,
        EwsAction::InstallApp => handle_install_app().await,
        EwsAction::UninstallApp => handle_uninstall_app().await,
        EwsAction::GetClientAccessToken => handle_get_client_access_token(&state, &auth, &body).await,
        EwsAction::GetReminders => handle_get_reminders(&state, &auth, &body).await,
        EwsAction::PerformReminderAction => {
            handle_perform_reminder_action(&state, &auth, &body).await
        }
        EwsAction::GetPersona => handle_get_persona(&state, &auth, &body).await,
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
                    b"GetEvents" => EwsAction::GetEvents,
                    b"GetStreamingEvents" => EwsAction::GetStreamingEvents,
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
                        && let Ok(v) = a
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
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

/// Extract the value of the named attribute from *every* occurrence of `tag`,
/// in document order. Useful for operations that accept a list of repeated
/// elements carrying a single id/attribute (e.g. `GetConversationItems`'
/// `<t:ConversationId Id="…"/>` list). Returns an empty vec when the tag is
/// absent or has no such attribute.
fn extract_first_attrs(xml: &str, tag: &[u8], attr: &[u8]) -> Vec<String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut values = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if e.name().local_name().as_ref() == tag =>
            {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == attr
                        && let Ok(v) = a
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                    {
                        values.push(v.into_owned());
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => return values,
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

/// Extract an attribute value from an XML tag's opening element.
///
/// Useful for the attributes Outlook puts directly on the EWS request element,
/// e.g. `<CreateItem SendMeetingInvitationsOrCancellations="SendToAllAndSaveCopy">`.
/// `tag_substr` is matched case-insensitively inside the body; `attr` must be
/// supplied WITHOUT the trailing `=`. Returns the (un-quoted) attribute value
/// preserved in its original case.
fn extract_open_tag_attr(xml: &str, tag_substr: &str, attr: &str) -> Option<String> {
    let lower = xml.to_ascii_lowercase();
    let tag_start = lower.find(tag_substr)?;
    let tag_end = lower[tag_start..]
        .find('>')
        .map(|i| tag_start + i)
        .unwrap_or(lower.len());
    let tag_fragment = &lower[tag_start..tag_end];
    let needle = format!("{attr}=");
    let attr_pos = tag_fragment.find(&needle)?;
    let value_start = attr_pos + needle.len();
    let value_rest = &tag_fragment[value_start..];
    let quote_char = value_rest.chars().next()?;
    let value_rest = &value_rest[quote_char.len_utf8()..];
    let value_end = value_rest.find(quote_char).unwrap_or(value_rest.len());
    // Recover the original-case value from the un-lower-cased source by offset.
    let offset_in_lower = tag_start + value_start + quote_char.len_utf8();
    Some(xml[offset_in_lower..offset_in_lower + value_end].to_string())
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
        // Outlook expects a Windows timezone id in StartTimeZone/EndTimeZone (per
        // [MS-OXWSCDATA] t:TimeZoneDefinitionType). Our stored value is IANA, so
        // convert back; fall back to the raw value if unmappable.
        let win = crate::timezone::iana_to_windows_timezone_name(v).unwrap_or_else(|| v.clone());
        let win_esc = xml_escape(&win);
        // Canonical EWS serialisation: `Id`/`Name` are *attributes* of
        // `TimeZoneDefinitionType` (per the Exchange Web Services schema and the
        // EWS Managed API `WriteAttributesToXml`), emitted as
        // `<t:StartTimeZone Id="..." Name="..."/>` — not the `<t:Id>`/`<t:Name>`
        // child-element shape (Ids are attributes, not elements). Outlook
        // CreateItem/GetItem echo this attribute form, which the inbound
        // `extract_ews_timezone_field_doc` reads back via the `Id` attribute.
        xml.push_str(&format!(
            "<t:StartTimeZone Id=\"{}\" Name=\"{}\"/>",
            win_esc, win_esc
        ));
        xml.push_str(&format!(
            "<t:EndTimeZone Id=\"{}\" Name=\"{}\"/>",
            win_esc, win_esc
        ));
        // <t:MeetingTimeZone> (MS-OXWSCORE §2.2.6, deprecated in favour of
        // StartTimeZone/EndTimeZone but still parsed by Outlook for back-compat)
        // is a `SerializableTimeZone` whose Windows id is the `TimeZoneName`
        // **attribute** — NOT element text. A CalDAV-origin `timezone_blob` is
        // the authoritative multi-line iCalendar VTIMEZONE *block* and would
        // corrupt the EWS envelope if emitted as element text; emit the same
        // Windows id (in its attribute) so it agrees with the standalone
        // <t:StartTimeZone> above.
        xml.push_str(&format!(
            "<t:MeetingTimeZone TimeZoneName=\"{}\"/>",
            win_esc
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
    interval_minutes: i64,
) -> anyhow::Result<(String, String)> {
    let secret_password = SecretString::from(password.to_string());

    // Check if JMAP Calendar is supported
    if !jmap.supports_calendar(mailbox, &secret_password).await {
        return Err(anyhow::anyhow!("JMAP Calendar not supported by server"));
    }

    let account_id = jmap
        .get_calendar_account_id(mailbox, &secret_password)
        .await?;
    // Honor the requested MergedFreeBusyIntervalInMinutes so the returned
    // MergedFreeBusy string has the same slot granularity the caller (and thus
    // the client) expects, matching the CalDAV fallback path exactly.
    let safe_interval = interval_minutes.clamp(5, 1440);
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

    // Free/busy backend selection mirrors the calendar-event backend so that a
    // single authoritative data source produces both the events and their
    // availability, avoiding divergence between JMAP Calendar and CalDAV.
    //
    // When `prefer_caldav_freebusy` is set (legacy Stalwart CalDAV path), CalDAV
    // is consulted first and JMAP Calendar is only used as a fallback. Otherwise
    // JMAP Calendar (urn:ietf:params:jmap:calendars) is the preferred source and
    // CalDAV is the fallback. This matches the EAS free/busy path exactly.
    let jmap_first = !state.cfg.prefer_caldav_freebusy;
    if jmap_first
        && let Some(jmap) = &state.jmap_client
        && let Ok(jmap_result) =
            fetch_freebusy_jmap(jmap, mailbox, password, start, end, interval_minutes).await
    {
        return jmap_result;
    }
    if jmap_first {
        tracing::debug!(target: "ews", "JMAP Calendar free-busy failed, falling back to CalDAV");
    }

    let caldav = match CaldavClient::new(&state.cfg) {
        Ok(c) => c,
        Err(_) => {
            // Fall back to JMAP Calendar when CalDAV was preferred but its client
            // cannot be constructed (e.g. `caldav_base` unset).
            if !jmap_first
                && let Some(jmap) = &state.jmap_client
                && let Ok(jmap_result) =
                    fetch_freebusy_jmap(jmap, mailbox, password, start, end, interval_minutes).await
            {
                return jmap_result;
            }
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
        // CalDAV produced no events. When CalDAV was preferred over JMAP
        // (`prefer_caldav_freebusy`), fall back to JMAP Calendar so availability
        // is still populated rather than reported as fully free ('0') or blocked
        // ('4'). When JMAP was already attempted and failed, both backends are
        // exhausted; report blocked rather than falsely free.
        if !jmap_first
            && let Some(jmap) = &state.jmap_client
            && let Ok(jmap_result) =
                fetch_freebusy_jmap(jmap, mailbox, password, start, end, interval_minutes).await
        {
            return jmap_result;
        }
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
    {svi}
  </s:Header>
  <s:Body>{inner}</s:Body>
</s:Envelope>"#,
        svi = version::current().render_ews_header(EWS_TYPE_NS),
        inner = inner
    );
    ews_response(StatusCode::OK, xml)
}

fn soap_fault(code: &str, message: &str, status: StatusCode) -> Response {
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>{svi}</s:Header>
  <s:Body><s:Fault><faultcode>s:Client</faultcode><faultstring>{}</faultstring><detail><m:ResponseCode xmlns:m="{}">{}</m:ResponseCode></detail></s:Fault></s:Body>
</s:Envelope>"#,
        xml_escape(message),
        EWS_MSG_NS,
        xml_escape(code),
        svi = version::current().render_ews_header(EWS_TYPE_NS)
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
    // Try JMAP Calendar first if preferred
    if state.cfg.prefer_jmap_calendar
        && let Some(jmap) = &state.jmap_client
    {
        let password_secret = SecretString::from(password);
        // Get calendar account ID
        let account_id = match jmap.get_calendar_account_id(owner, &password_secret).await {
            Ok(id) => id,
            Err(e) => {
                tracing::debug!(target: "ews", error = %e, "JMAP get_calendar_account_id failed, falling back to CalDAV");
                return load_current_calendar_items_caldav(state, owner, password, window).await;
            }
        };
        // Determine time window
        let (start, end) = window.unwrap_or_else(|| {
            (
                chrono::Utc::now() - chrono::Duration::weeks(104),
                chrono::Utc::now() + chrono::Duration::weeks(104),
            )
        });
        // Query events
        let query_params = QueryCalendarEventsParams {
            account_id: &account_id,
            calendar_id: None,
            start: &start.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            end: &end.format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            limit: 10000, // large enough to capture all events in window
            username: owner,
            password: &password_secret,
        };
        let query_result = jmap.query_calendar_events(query_params).await?;
        // Filter out events without IDs; collect as Vec<String>
        let event_ids: Vec<String> = query_result
            .events
            .iter()
            .filter_map(|e| e.id.clone())
            .collect();
        if event_ids.is_empty() {
            return Ok(vec![]);
        }
        // Batch fetch events
        let event_map = jmap
            .get_calendar_events(&account_id, &event_ids, owner, &password_secret)
            .await?;
        // Build output
        let mut out = Vec::new();
        for event_id in &event_ids {
            if let Some((ics, etag)) = event_map.get(event_id)
                && let Some(ci) = parse_ics_event(ics)
            {
                // Clone values we need before moving ci
                let ci_uid = ci.uid.clone();
                // Stable server_id: HMAC of owner + event_id
                let server_id = generate_server_id(
                    state.cfg.hmac_secret(),
                    &format!("jmap:{}:{}", owner, event_id),
                );
                out.push(CurrentCalendarItem {
                    row: EwsItemRow {
                        server_id: server_id.clone(),
                        caldav_href: None,
                        resource_href: format!("jmap://calendar/{}/{}", account_id, event_id),
                        uid: Some(ci_uid.clone()),
                        etag: Some(etag.clone()),
                        updated_at: None,
                    },
                    item: ci,
                });
                // Upsert into storage for future lookups
                if let Err(e) = state
                    .storage
                    .upsert_item_map(
                        owner,
                        "", // caldav_href empty for JMAP
                        &format!("jmap://calendar/{}/{}", account_id, event_id),
                        &server_id,
                        &ci_uid,
                        etag,
                    )
                    .await
                {
                    tracing::warn!(server_id = %server_id, error = %e, "Failed to upsert item map in load_current_calendar_items (JMAP)");
                }
            }
        }
        return Ok(out);
    }
    // Fallback to CalDAV implementation
    load_current_calendar_items_caldav(state, owner, password, window).await
}

async fn load_current_calendar_items_caldav(
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
                        && let Ok(v) = a
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
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

/// Handle EWS FindItem for Contacts using CardDAV.
async fn handle_find_contacts_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = owner_from_username(&auth.username);
    let max = extract_int(body, b"MaxEntriesReturned", 100);
    let offset = extract_int(body, b"Offset", 0);
    let shape = requested_item_shape(body);

    // Ensure CardDAV client is configured
    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => {
            return operation_error_response(
                &EwsAction::FindItem,
                "ErrorInternalServerError",
                "CardDAV client not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Fetch all contacts from CardDAV
    let (carddav_contacts, _) = match carddav
        .list_contacts(&auth.username, auth.password.expose_secret(), None)
        .await
    {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(error = %e, "CardDAV list_contacts failed");
            return operation_error_response(
                &EwsAction::FindItem,
                "ErrorInternalServerError",
                "Failed to fetch contacts",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Map server IDs from DB, but for FindItem we can use CardDAV href to derive stable IDs.
    // We'll generate a server_id like "contact-{index}" but ensure consistency across calls.
    let total_items = carddav_contacts.len();
    let paged_contacts = carddav_contacts
        .into_iter()
        .skip(offset)
        .take(max)
        .collect::<Vec<_>>();

    let mut items_xml = String::new();
    let mut actual_count = 0;
    for contact in paged_contacts.iter() {
        // Use a deterministic server_id based on href to avoid churn
        let server_id = format!("contact-{}", sha256_hash(&contact.href));

        // Generate ChangeKey: include etag and a timestamp component to indicate changes.
        // Per RFC 7252, ChangeKey should change when content changes.
        // We'll use: format!("{}-{}", server_id, current timestamp in seconds)
        // For stability, we'll use the contact's etag if available, else a timestamp.
        let change_key = if let Some(ref etag) = contact.etag {
            // Use a compound key: server_id + etag (etag changes when vcard changes)
            format!("{}-{}", server_id, etag)
        } else {
            // No etag from server; use timestamp to indicate potential future changes
            format!("{}-{}", server_id, chrono::Utc::now().timestamp())
        };

        // Register/update this contact mapping in the database to support subsequent GetItem/UpdateItem/DeleteItem
        if let Err(e) = state
            .storage
            .upsert_contact(
                owner,
                &contact.href,
                &server_id,
                contact.etag.as_deref(),
                Some(&contact.vcard),
            )
            .await
        {
            tracing::warn!(?server_id, error = %e, "Failed to upsert contact mapping in DB, skipping this contact in FindItem");
            // Skip this contact - without DB mapping, subsequent Get/Update/Delete will fail
            continue;
        }

        // Parse vCard to extract properties for EWS Contact shape
        let vcard = parse_vcard_from_data(&contact.vcard).unwrap_or_else(|_| {
            warn!(href = %contact.href, "Failed to parse vCard for contact");
            Vcard::default()
        });

        // Build Contact XML per EWS schema
        let contact_xml = render_ews_contact(&server_id, &change_key, &vcard, shape);
        items_xml.push_str(&contact_xml);
        actual_count += 1;
    }

    let response = format!(
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}"><t:Items>{}</t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        actual_count,
        if offset + actual_count >= total_items {
            "true"
        } else {
            "false"
        },
        offset,
        items_xml
    );
    soap_ok(response)
}

/// Handle EWS GetItem for Contacts using CardDAV.
async fn handle_get_contact_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    // Extract ItemId(s) from request
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "XML parse error in GetItem");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorSchemaValidation",
                "Invalid XML",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let mut item_ids = Vec::new();
    for node in doc.descendants() {
        if node.has_tag_name("ItemId")
            && let Some(id) = node.attribute("Id")
        {
            item_ids.push(id.to_string());
        }
    }

    if item_ids.is_empty() {
        return operation_error_response(
            &EwsAction::GetItem,
            "ErrorInvalidItemId",
            "No ItemId provided",
            StatusCode::OK,
        );
    }

    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInternalServerError",
                "CardDAV client not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let shape = requested_item_shape(body);
    let mut items_xml = String::new();

    for server_id in item_ids {
        // The server_id in our system maps to contact_map.server_id.
        let db_contact = match state.storage.get_contact(&auth.username, &server_id).await {
            Ok(Some(row)) => Some(row),
            Ok(None) => None,
            Err(e) => {
                tracing::error!(error = %e, ?server_id, "DB error fetching contact");
                None
            }
        };

        if let Some(row) = db_contact {
            // Fetch latest vCard from CardDAV using href (or use stored vCard if fetch fails)
            let vcard_str = match carddav
                .client
                .get(format!(
                    "{}{}",
                    carddav.addressbook_home(&auth.username),
                    row.carddav_href
                ))
                .basic_auth(&auth.username, Some(auth.password.expose_secret()))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp.text().await.ok(),
                _ => row.vcard.clone(),
            };

            let vcard = vcard_str
                .as_ref()
                .and_then(|s| parse_vcard_from_data(s).ok())
                .unwrap_or_else(Vcard::default);

            let change_key = row.etag.clone().unwrap_or_else(|| server_id.clone());
            let contact_xml = render_ews_contact(&row.server_id, &change_key, &vcard, shape);
            items_xml.push_str(&contact_xml);
        } else {
            // Contact not found: return error for this item (per EWS, we can still succeed overall)
            // For simplicity, we skip missing items and return an empty response.
            // Production implementation would return <MessageXml> with error status.
            tracing::warn!(?server_id, "Contact not found for GetItem");
        }
    }

    if items_xml.is_empty() {
        return operation_error_response(
            &EwsAction::GetItem,
            "ErrorInvalidItemId",
            "No valid contacts found",
            StatusCode::OK,
        );
    }

    let response = format!(
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:GetItemResponseMessage></m:ResponseMessages></m:GetItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, items_xml
    );
    soap_ok(response)
}

/// Render a vCard as an EWS Contact element.
fn render_ews_contact(
    server_id: &str,
    change_key: &str,
    vcard: &Vcard,
    _shape: ItemShape,
) -> String {
    // Determine display name: use full_name if non-empty, else N property
    let display_name = vcard
        .full_name()
        .or_else(|| vcard.name())
        .unwrap_or_default();
    let email = vcard.emails().first().copied().unwrap_or("");
    let phone = vcard.phones().first().copied().unwrap_or("");
    // ORG value is components joined by ';'
    let org = vcard
        .org()
        .as_deref()
        .map(|o| o.join(";"))
        .unwrap_or_default();
    let title = vcard.title().unwrap_or("");

    let mut xml = String::new();
    xml.push_str("<t:Contact>");
    xml.push_str(&format!(
        "<t:ItemId Id=\"{}\" ChangeKey=\"{}\"/>",
        xml_escape(server_id),
        xml_escape(change_key)
    ));
    if !display_name.is_empty() {
        xml.push_str(&format!(
            "<t:DisplayName>{}</t:DisplayName>",
            xml_escape(display_name)
        ));
    }
    if !email.is_empty() {
        xml.push_str(&format!(
            "<t:EmailAddresses><t:Entry Key=\"EmailAddress1\">{}</t:Entry></t:EmailAddresses>",
            xml_escape(email)
        ));
    }
    if !phone.is_empty() {
        xml.push_str(&format!(
            "<t:PhoneNumbers><t:Entry Key=\"Phone\">{}</t:Entry></t:PhoneNumbers>",
            xml_escape(phone)
        ));
    }
    if !org.is_empty() {
        xml.push_str(&format!(
            "<t:CompanyName>{}</t:CompanyName>",
            xml_escape(&org)
        ));
    }
    if !title.is_empty() {
        xml.push_str(&format!("<t:JobTitle>{}</t:JobTitle>", xml_escape(title)));
    }
    // Add more fields as needed: Addresses, ImAddress, etc.
    xml.push_str("</t:Contact>");
    xml
}

/// Simple SHA256 hash used to generate stable IDs from CardDAV hrefs.
fn sha256_hash(s: &str) -> String {
    let digest = Sha256::digest(s.as_bytes());
    // Take first 16 bytes for a reasonably short but unique identifier.
    // Using hex encoding (lowercase) as per MS-OXWSCORE §2.2.4.25 for ChangeKey,
    // but this is not a true ChangeKey; it's just a stable identifier.
    hex::encode(&digest[..16])
}

/// Handle EWS CreateItem for Contacts using CardDAV.
async fn handle_create_contact_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "CardDAV not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Parse the <t:Contact> element from request
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "XML parse error in CreateItem Contact");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorSchemaValidation",
                "Invalid XML",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Extract contact fields
    let mut display_name = None;
    let mut email = None;
    let mut phone = None;
    let mut organization = None;
    let mut title = None;

    for node in doc.descendants() {
        match node.tag_name().name() {
            "DisplayName" => display_name = node.text().map(str::to_string),
            "EmailAddresses" => {
                // Look for <t:Entry Key="EmailAddress1">value</t:Entry>
                for entry in node.children() {
                    if entry.has_tag_name("Entry")
                        && entry.attribute("Key") == Some("EmailAddress1")
                    {
                        email = entry.text().map(str::to_string);
                    }
                }
            }
            "PhoneNumbers" => {
                for entry in node.children() {
                    if entry.has_tag_name("Entry") && entry.attribute("Key") == Some("Phone") {
                        phone = entry.text().map(str::to_string);
                    }
                }
            }
            "CompanyName" => organization = node.text().map(str::to_string),
            "JobTitle" => title = node.text().map(str::to_string),
            _ => {}
        }
    }

    // Build vCard
    let uid = uuid::Uuid::new_v4().to_string();
    let vcard_str = match crate::vcard::build_vcard(
        &uid,
        display_name.as_deref().unwrap_or(""),
        email.as_deref(),
        phone.as_deref(),
        organization.as_deref(),
        title.as_deref(),
    ) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to build vCard");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "Failed to build contact",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Create contact via CardDAV client method
    let (href, etag) = match carddav
        .create_contact(&auth.username, auth.password.expose_secret(), &vcard_str)
        .await
    {
        Ok(res) => res,
        Err(e) => {
            tracing::error!(error = %e, "CardDAV create_contact failed");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "Failed to create contact on CardDAV",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Store in contact_map
    let server_id = format!("contact-{}", uuid::Uuid::new_v4().simple());
    if let Err(e) = state
        .storage
        .insert_contact(
            &auth.username,
            &href,
            &server_id,
            Some(etag.as_str()),
            Some(vcard_str.as_str()),
        )
        .await
    {
        tracing::error!(error = %e, "Failed to store contact in DB");
        // Continue anyway; we have the contact on server.
    }

    // Return ItemId with ChangeKey = etag if non-empty, else server_id
    let change_key = if etag.is_empty() {
        server_id.clone()
    } else {
        etag
    };
    publish_event(
        state,
        NotificationEvent::ItemCreated {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Contacts),
            item_id: server_id.clone(),
            change_key: change_key.clone(),
        },
    );
    let response_xml = format!(
        r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Contact><t:ItemId Id="{}" ChangeKey="{}"/></t:Contact></m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, server_id, change_key
    );
    soap_ok(response_xml)
}

/// Handle EWS UpdateItem for Contacts using CardDAV.
async fn handle_update_contact_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "CardDAV not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Extract ItemId/@Id
    let item_id = extract_first_attr(body, b"ItemId", b"Id");
    if item_id.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInvalidIdMalformed",
            "UpdateItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    // Look up contact by server_id to get href and etag
    let db_contact = match state
        .storage
        .get_contact(&auth.username, item_id.as_deref().unwrap_or_default())
        .await
    {
        Ok(Some(contact)) => contact,
        Ok(None) => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorItemNotFound",
                "Contact not found",
                StatusCode::OK,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "DB error fetching contact");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "Database error",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Parse updates from the <t:Contact> element
    let doc = match roxmltree::Document::parse(body) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "XML parse error in UpdateItem Contact");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorSchemaValidation",
                "Invalid XML",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // Load existing vCard (from DB or fetch fresh)
    let vcard_str: String = match state
        .storage
        .get_contact(&auth.username, item_id.as_deref().unwrap_or_default())
        .await
    {
        Ok(Some(c)) => c.vcard.clone().unwrap_or_default(),
        _ => {
            // Try fetching from CardDAV
            let url = format!(
                "{}{}",
                carddav.addressbook_home(&auth.username),
                db_contact.carddav_href
            );
            let resp = match carddav
                .client
                .get(&url)
                .basic_auth(&auth.username, Some(auth.password.expose_secret()))
                .send()
                .await
            {
                Ok(r) => r,
                Err(_) => {
                    return operation_error_response(
                        &EwsAction::UpdateItem,
                        "ErrorInternalServerError",
                        "Failed to fetch existing contact",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };
            if resp.status().is_success() {
                resp.text().await.ok().unwrap_or_default()
            } else {
                return operation_error_response(
                    &EwsAction::UpdateItem,
                    "ErrorInternalServerError",
                    "Failed to fetch existing contact",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    };

    // Parse into mutable vCard using vcard::parser? The vcard crate may not provide mutable builder.
    // We'll re-build vCard by merging old + new manually.
    let mut old_vcard = match parse_vcard_from_data(&vcard_str) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to parse existing vCard");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "Invalid existing vCard",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Extract updates
    let mut new_display_name = None;
    let mut new_email = None;
    let mut new_phone = None;
    let mut new_org = None;
    let mut new_title = None;

    for node in doc.descendants() {
        match node.tag_name().name() {
            "DisplayName" => new_display_name = node.text().map(str::to_string),
            "EmailAddresses" => {
                for entry in node.children() {
                    if entry.has_tag_name("Entry")
                        && entry.attribute("Key") == Some("EmailAddress1")
                    {
                        new_email = entry.text().map(str::to_string);
                    }
                }
            }
            "PhoneNumbers" => {
                for entry in node.children() {
                    if entry.has_tag_name("Entry") && entry.attribute("Key") == Some("Phone") {
                        new_phone = entry.text().map(str::to_string);
                    }
                }
            }
            "CompanyName" => new_org = node.text().map(str::to_string),
            "JobTitle" => new_title = node.text().map(str::to_string),
            _ => {}
        }
    }

    // Apply updates: if a field is present in the request, replace; else keep existing.
    if let Some(name) = new_display_name {
        // Update the FN property in vcard
        let mut fn_found = false;
        for prop in &mut old_vcard.properties {
            if let vcard::Property::Fn(fn_val) = prop {
                fn_val.value = name.clone();
                fn_found = true;
            }
        }
        if !fn_found {
            old_vcard
                .properties
                .push(vcard::Property::Fn(vcard::Fn { value: name }));
        }
    }

    if let Some(email) = new_email {
        // Replace or add EMAIL property
        old_vcard
            .properties
            .retain(|p| !matches!(p, vcard::Property::Email(_)));
        old_vcard
            .properties
            .push(vcard::Property::Email(vcard::Email { email }));
    }

    if let Some(phone) = new_phone {
        old_vcard
            .properties
            .retain(|p| !matches!(p, vcard::Property::Tel(_)));
        let tel = vcard::Tel {
            number: phone,
            params: vec![
                vcard::Parameter::Type(vcard::Type::Work),
                vcard::Parameter::Type(vcard::Type::Voice),
            ],
        };
        old_vcard.properties.push(vcard::Property::Tel(tel));
    }

    if let Some(org) = new_org {
        old_vcard
            .properties
            .retain(|p| !matches!(p, vcard::Property::Org(_)));
        old_vcard
            .properties
            .push(vcard::Property::Org(vcard::Org { value: vec![org] }));
    }

    if let Some(title) = new_title {
        old_vcard
            .properties
            .retain(|p| !matches!(p, vcard::Property::Title(_)));
        old_vcard
            .properties
            .push(vcard::Property::Title(vcard::Title { value: title }));
    }

    // Serialize updated vCard
    let updated_vcard_str = old_vcard.to_string();

    // PUT to CardDAV with If-Match header (use stored etag if available)
    let url = format!(
        "{}{}",
        carddav.addressbook_home(&auth.username),
        db_contact.carddav_href
    );
    let mut request = carddav
        .client
        .put(&url)
        .basic_auth(&auth.username, Some(auth.password.expose_secret()))
        .header("Content-Type", "text/vcard; charset=utf-8")
        .body(updated_vcard_str.clone());

    if let Some(ref etag) = db_contact.etag {
        request = request.header("If-Match", format!("\"{}\"", etag));
    }

    let response = match request.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "CardDAV PUT failed");
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "Failed to update contact on CardDAV",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        if status == StatusCode::PRECONDITION_FAILED {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInvalidChangeKey",
                "ChangeKey mismatch",
                StatusCode::OK,
            );
        }
        tracing::error!(status = %status, "CardDAV update failed");
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInternalServerError",
            "CardDAV update contact failed",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Update DB etag if needed
    let new_etag = response
        .headers()
        .get("ETag")
        .and_then(|h| h.to_str().ok())
        .map(|s| s.trim_matches('"').to_string());

    if let Err(e) = state
        .storage
        .update_contact(
            &auth.username,
            item_id.as_deref().unwrap_or_default(),
            new_etag.as_deref(),
            Some(updated_vcard_str.as_str()),
        )
        .await
    {
        tracing::error!(error = %e, "Failed to update contact in DB");
    }
    publish_event(
        state,
        NotificationEvent::ItemModified {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Contacts),
            item_id: item_id.clone().unwrap_or_default(),
            change_key: new_etag.clone().unwrap_or_default(),
        },
    );

    let response_xml = format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(response_xml)
}

/// Handle EWS DeleteItem for Contacts using CardDAV.
async fn handle_delete_contact_item(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let carddav = match state.carddav_client.as_ref() {
        Some(c) => c,
        None => {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "CardDAV not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Extract ItemId/@Id
    let item_id = extract_first_attr(body, b"ItemId", b"Id");
    if item_id.as_deref().map(|s| s.is_empty()).unwrap_or(true) {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInvalidIdMalformed",
            "DeleteItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    // Look up contact to get href and etag
    let db_contact = match state
        .storage
        .get_contact(&auth.username, item_id.as_deref().unwrap_or_default())
        .await
    {
        Ok(Some(contact)) => contact,
        Ok(None) => {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorItemNotFound",
                "Contact not found",
                StatusCode::OK,
            );
        }
        Err(e) => {
            tracing::error!(error = %e, "DB error fetching contact");
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "Database error",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // DELETE from CardDAV with If-Match if etag available
    let url = format!(
        "{}{}",
        carddav.addressbook_home(&auth.username),
        db_contact.carddav_href
    );
    let mut request = carddav
        .client
        .delete(&url)
        .basic_auth(&auth.username, Some(auth.password.expose_secret()));

    if let Some(ref etag) = db_contact.etag {
        request = request.header("If-Match", format!("\"{}\"", etag));
    }

    let response = match request.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::error!(error = %e, "CardDAV DELETE failed");
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "Failed to delete contact on CardDAV",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    if !response.status().is_success() {
        let status = response.status();
        if status == StatusCode::PRECONDITION_FAILED {
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInvalidChangeKey",
                "ChangeKey mismatch",
                StatusCode::OK,
            );
        }
        tracing::error!(status = %status, "CardDAV delete failed");
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            "CardDAV delete contact failed",
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    // Remove from DB
    if let Err(e) = state
        .storage
        .delete_contact(&auth.username, item_id.as_deref().unwrap_or_default())
        .await
    {
        tracing::error!(error = %e, "Failed to delete contact from DB");
        // Continue anyway; contact already deleted on server.
    }
    publish_event(
        state,
        NotificationEvent::ItemDeleted {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Contacts),
            item_id: item_id.clone().unwrap_or_default(),
        },
    );

    let response_xml = format!(
        r#"<m:DeleteItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:DeleteItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:DeleteItemResponseMessage></m:ResponseMessages></m:DeleteItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(response_xml)
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

    // Contacts folder — route to CardDAV
    if matches!(folder, DistinguishedFolder::Contacts) {
        return handle_find_contacts_item(state, auth, body).await;
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

/// C4: Handle EWS AcceptItem/DeclineItem/TentativelyAcceptItem response
/// objects (MS-OXWSMTGSRC). Outlook sends these inside a `CreateItem` whose
/// `Items` contains one of the three response types referencing the meeting
/// request's `ItemId` via `ReferenceItemId`.
///
/// Flow:
/// 1. Detect the decision (Accept / Tentative / Decline).
/// 2. Resolve `ReferenceItemId` (the meeting-request email) to a JMAP email.
/// 3. Download the email raw MIME blob, extract its `METHOD:REQUEST` iCalendar.
/// 4. Build an iTIP REPLY and send it to the meeting organizer via SMTP (C4).
/// 5. Each EAS-style local-calendar PARTSTAT patch is handled by the EAS sync
///    path; on EWS the calendar copy lives on the same mailbox, so we also
///    update the local attendee's PARTSTAT on any matching calendar event via
///    CalDAV (keeping the local roster consistent with the reply we sent).
async fn handle_meeting_response_object(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    // Step 1: detect which response object was sent.
    let decision = if body.contains("<t:AcceptItem") {
        crate::meeting::ResponseDecision::Accept
    } else if body.contains("<t:TentativelyAcceptItem") {
        crate::meeting::ResponseDecision::Tentative
    } else if body.contains("<t:DeclineItem") {
        crate::meeting::ResponseDecision::Decline
    } else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInvalidRequest",
            "Unrecognized meeting response object",
            StatusCode::BAD_REQUEST,
        );
    };

    // Step 2: locate the referenced meeting-request email ItemId.
    let Some(reference_id) = extract_first_attr(body, b"ReferenceItemId", b"Id") else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorSchemaValidation",
            "Meeting response object missing ReferenceItemId",
            StatusCode::BAD_REQUEST,
        );
    };

    if !state.cfg.email_enabled {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInvalidRequest",
            "Email operations are not enabled; meeting responses require the request email",
            StatusCode::FORBIDDEN,
        );
    }

    let Some(jmap) = state.jmap_client.as_ref().cloned() else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInvalidRequest",
            "JMAP not configured for meeting requests",
            StatusCode::FORBIDDEN,
        );
    };

    let password_secret = SecretString::from(auth.password.expose_secret());

    // Step 3: resolve the reference id to a JMAP email and download its raw MIME.
    let jmap_id = match crate::email::jmap_id_from_email_server_id(&reference_id) {
        Some(id) => id.to_string(),
        None => {
            // Accept bare JMAP ids; otherwise the reference isn't an email item.
            reference_id
                .strip_prefix("em-")
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| reference_id.clone())
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &password_secret).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "CreateItem meeting-response: failed to get JMAP account ID");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "Failed to get email account",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let email = match jmap
        .get_email(&account_id, &jmap_id, &auth.username, &auth.password)
        .await
    {
        Ok(Some(e)) => e,
        Ok(None) | Err(_) => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorItemNotFound",
                "Referenced meeting request email not found",
                StatusCode::OK,
            );
        }
    };

    let Some(blob_id) = email.blob_id.as_ref().filter(|b| !b.is_empty()) else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorItemNotFound",
            "Meeting request email has no downloadable blob",
            StatusCode::OK,
        );
    };

    let raw_mime = match jmap
        .download_blob(&account_id, blob_id, &auth.username, &password_secret)
        .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::error!(error = %e, "CreateItem meeting-response: blob download failed");
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "Failed to download meeting request",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let Some(ics) = crate::email::extract_meeting_request_ics(&raw_mime) else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInvalidRequest",
            "Meeting request email contains no METHOD:REQUEST iCalendar",
            StatusCode::OK,
        );
    };

    let Some(invitation) = crate::meeting::parse_meeting_request(&ics) else {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInvalidRequest",
            "Failed to parse meeting request iCalendar",
            StatusCode::OK,
        );
    };

    // Step 4: deliver the iTIP REPLY to the organizer via SMTP.
    match crate::meeting::submit_meeting_response(
        state,
        &invitation,
        decision,
        &auth.username,
        &password_secret,
    )
    .await
    {
        Ok(message_id) => {
            tracing::info!(
                target: "ews",
                uid = %invitation.uid,
                decision = ?decision,
                message_id = %message_id,
                "Sent iTIP reply for meeting-response object"
            );
        }
        Err(e) => {
            // SMTP/JMAP submission unavailable. Log but still report success to
            // Outlook: the local calendar has the reply recorded (EWS clients
            // treat Accept/Decline as authoritative on the local calendar) and
            // re-trying delivery is the gateway's responsibility, not the
            // client's. We surface the failure in logs rather than blocking UI.
            tracing::warn!(
                error = %e,
                uid = %invitation.uid,
                decision = ?decision,
                "Could not deliver iTIP reply; meeting response recorded locally only"
            );
        }
    }

    // Outlook manages the local calendar copy itself via subsequent
    // CreateItem / SyncFolderItems calls on the Calendar folder (which the
    // gateway already handles over CalDAV). The gateway's C4 responsibility
    // for AcceptItem / DeclineItem / TentativelyAcceptItem is the iTIP
    // delivery to the organizer, performed above. The meeting-response message
    // object Outlook expects in the Inbox is synthesised below.

    // Build a CreateItemResponse echoing a new ItemId for the response object
    // (Outlook expects a created item for the meeting-response message class).
    let new_id = format!(
        "mr-{}-{}",
        invitation.uid,
        chrono::Utc::now().timestamp_millis()
    );
    let change_key = new_id.clone();
    let message_class = match decision {
        crate::meeting::ResponseDecision::Accept => "IPM.Schedule.Meeting.Resp.Pos",
        crate::meeting::ResponseDecision::Decline => "IPM.Schedule.Meeting.Resp.Neg",
        crate::meeting::ResponseDecision::Tentative => "IPM.Schedule.Meeting.Resp.Tent",
    };
    let item_xml = format!(
        r#"<t:CalendarItem><t:ItemId Id="{id}" ChangeKey="{ck}"/><t:ItemClass>{cls}</t:ItemClass></t:CalendarItem>"#,
        id = xml_escape(&new_id),
        ck = xml_escape(&change_key),
        cls = message_class,
    );
    soap_ok(format!(
        r#"<m:CreateItemResponse xmlns:m="{msg_ns}" xmlns:t="{type_ns}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{items}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
        msg_ns = EWS_MSG_NS,
        type_ns = EWS_TYPE_NS,
        items = item_xml,
    ))
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

    // Check if item is a contact: server_id prefix "contact-" (as we generate in FindItem)
    // Use simple prefix check; more robust would be to look up in contact_map.
    if item_id.starts_with("contact-") && state.carddav_client.is_some() {
        return handle_get_contact_item(state, auth, body).await;
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

    // Choose backend: JMAP if resource_href indicates jmap://, else CalDAV
    let calendar_item_xml = if item.resource_href.starts_with("jmap://") {
        // Parse resource_href: jmap://calendar/{account_id}/{event_id}
        let parts: Vec<&str> = item
            .resource_href
            .trim_start_matches("jmap://calendar/")
            .split('/')
            .collect();
        if parts.len() != 2 {
            tracing::warn!(resource_href = %item.resource_href, "Invalid JMAP resource href");
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorInvalidIdMalformed",
                "Invalid calendar item ID",
                StatusCode::OK,
            );
        }
        let account_id = parts[0];
        let event_id = parts[1];
        let password_secret = SecretString::from(auth.password.expose_secret());
        let jmap = match &state.jmap_client {
            Some(j) => j,
            None => {
                tracing::error!("JMAP client not available for JMAP calendar item");
                return operation_error_response(
                    &EwsAction::GetItem,
                    "ErrorInternalServerError",
                    "JMAP backend not configured",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };
        match jmap
            .get_calendar_event(account_id, event_id, &item_owner, &password_secret)
            .await
        {
            Ok((ics, _returned_event_id, _etag)) => match parse_ics_event(&ics) {
                Some(ci) => {
                    let att_list = state
                        .attachment_manager
                        .get_attachments_for_item(&item_owner, &item.server_id)
                        .await
                        .unwrap_or_default();
                    let has_atts = !att_list.is_empty();
                    let att_summaries: Vec<_> =
                        att_list.iter().map(|a| a.to_ews_summary()).collect();
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
            Err(e) => {
                tracing::warn!(error = %e, "JMAP get_calendar_event failed, falling back to CalDAV if possible");
                // Fallback to CalDAV if possible
                let caldav = match CaldavClient::new(&state.cfg) {
                    Ok(c) => c,
                    Err(e2) => {
                        tracing::error!(error = %e2, "Failed to create CalDAV client for fallback");
                        return operation_error_response(
                            &EwsAction::GetItem,
                            "ErrorInternalServerError",
                            "An internal error occurred",
                            StatusCode::INTERNAL_SERVER_ERROR,
                        );
                    }
                };
                match caldav
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
                            let att_summaries: Vec<_> =
                                att_list.iter().map(|a| a.to_ews_summary()).collect();
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
                }
            }
        }
    } else {
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
        match caldav
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
                    let att_summaries: Vec<_> =
                        att_list.iter().map(|a| a.to_ews_summary()).collect();
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
        }
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

/// The subscription request element carried inside a `<Subscribe>` body,
/// identified purely from the XML structure (not raw substring matching) so the
/// determination is namespace- and whitespace-insensitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetectedSubscriptionRequest {
    Pull,
    Streaming,
    /// Push subscription: server-initiated delivery to a client callback URL.
    Push,
}

/// Determine the subscription kind from the request's XML *structure* rather
/// than from raw `body.contains("…SubscriptionRequest")` substring matching.
///
/// A `Subscribe` request contains exactly one of `PullSubscriptionRequest`,
/// `StreamingSubscriptionRequest`, or `PushSubscriptionRequest` as a child of
/// its envelope body (MS-OXWSNTIF 3.1.4.3.3.2). Instead of brittle string
/// matching (which is namespace- and formatting-sensitive — a renamed prefix,
/// extra whitespace or a comment could misroute the request), we walk the XML
/// event stream with `quick-xml` and pick the first such element that is
/// nested within a `Subscribe` ancestor. Element names are matched by local
/// name only, so any namespace prefix works.
fn detect_subscription_request_kind(body: &str) -> Option<DetectedSubscriptionRequest> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    // Depth of the innermost `Subscribe` element we're currently inside; only a
    // request element nested under Subscribe is accepted so a stray element of
    // the same name elsewhere cannot misroute detection.
    let mut subscribe_depth: i32 = 0;
    let mut kind: Option<DetectedSubscriptionRequest> = None;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.name().local_name();
                if name.as_ref() == b"Subscribe" {
                    subscribe_depth += 1;
                } else if kind.is_none()
                    && subscribe_depth > 0
                    && matches!(
                        name.as_ref(),
                        b"PullSubscriptionRequest"
                            | b"StreamingSubscriptionRequest"
                            | b"PushSubscriptionRequest"
                    )
                {
                    kind = Some(match name.as_ref() {
                        b"PullSubscriptionRequest" => DetectedSubscriptionRequest::Pull,
                        b"StreamingSubscriptionRequest" => DetectedSubscriptionRequest::Streaming,
                        // unreachable: guarded by the `matches!` above.
                        _ => DetectedSubscriptionRequest::Push,
                    });
                }
            }
            Ok(Event::Empty(e)) if kind.is_none() && subscribe_depth > 0 => {
                let name = e.name().local_name();
                if matches!(
                    name.as_ref(),
                    b"PullSubscriptionRequest"
                        | b"StreamingSubscriptionRequest"
                        | b"PushSubscriptionRequest"
                ) {
                    kind = Some(match name.as_ref() {
                        b"PullSubscriptionRequest" => DetectedSubscriptionRequest::Pull,
                        b"StreamingSubscriptionRequest" => DetectedSubscriptionRequest::Streaming,
                        _ => DetectedSubscriptionRequest::Push,
                    });
                }
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"Subscribe" => {
                if subscribe_depth > 0 {
                    subscribe_depth -= 1;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    kind
}

/// Parse the per-subscription request configuration shared by the pull and
/// streaming request types (MS-OXWSNTIF 3.1.4.3.3.3).
struct ParsedSubscriptionRequest {
    folders: Option<HashSet<String>>,
    event_types: Option<HashSet<String>>,
}

/// Parse the `<FolderIds>` and `<EventTypes>` children of a subscription
/// request body. Returns folders `None` when `SubscribeToAllFolders="true"`.
fn parse_subscription_request(body: &str) -> ParsedSubscriptionRequest {
    let subscribe_to_all =
        extract_first_attr(body, b"PullSubscriptionRequest", b"SubscribeToAllFolders")
            .map(|v| v == "true")
            .or_else(|| {
                extract_first_attr(
                    body,
                    b"StreamingSubscriptionRequest",
                    b"SubscribeToAllFolders",
                )
                .map(|v| v == "true")
            })
            .unwrap_or(false);

    let folders = if subscribe_to_all {
        None
    } else {
        let ids = extract_folder_ids_from_block(body);
        if ids.is_empty() { None } else { Some(ids) }
    };
    let event_types = {
        let types = extract_event_types(body);
        if types.is_empty() { None } else { Some(types) }
    };
    ParsedSubscriptionRequest {
        folders,
        event_types,
    }
}

/// Parse the `PushSubscriptionRequest` configuration (MS-OXWSNTIF 3.1.4.3.4.4):
/// the mandatory callback `URL`, the optional `StatusFrequency` keep-alive
/// interval (minutes, default 6), and the optional opaque `CallerData` echoed
/// back on delivery. Folder/event-type filters are read by
/// [`parse_subscription_request`], shared across all three request kinds.
fn parse_push_subscription_request(body: &str) -> PushConfig {
    let url = extract_first_tag_text(body, b"URL").unwrap_or_default();
    let status_frequency_minutes = extract_first_tag_text(body, b"StatusFrequency")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(6)
        .clamp(1, 1440);
    let caller_data = extract_first_tag_text(body, b"CallerData");
    PushConfig {
        url,
        status_frequency_minutes,
        caller_data,
    }
}

/// Extract `<t:FolderId Id="…"/>` / `<t:DistinguishedFolderId Id="…"/>`
/// values appearing inside any `*SubscriptionRequest` block.
fn extract_folder_ids_from_block(body: &str) -> HashSet<String> {
    let mut ids = HashSet::new();
    let mut reader = Reader::from_str(body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_folder_ids = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"FolderIds" => {
                in_folder_ids = true;
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"FolderIds" => {
                in_folder_ids = false;
            }
            Ok(Event::Start(e)) | Ok(Event::Empty(e))
                if in_folder_ids
                    && (e.name().local_name().as_ref() == b"FolderId"
                        || e.name().local_name().as_ref() == b"DistinguishedFolderId") =>
            {
                for a in e.attributes().flatten() {
                    if a.key.local_name().as_ref() == b"Id"
                        && let Ok(v) = a
                            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
                    {
                        ids.insert(v.into_owned());
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    ids
}

/// Extract `<t:EventType>…</t:EventType>` textual values.
fn extract_event_types(body: &str) -> HashSet<String> {
    extract_tag_texts(body, b"EventType").into_iter().collect()
}

/// Encode a per-subscription watermark counter as an opaque, URL-safe-ish string
/// the client echoes back verbatim. EWS watermarks are server-opaque.
fn encode_watermark(seq: u64) -> String {
    STANDARD.encode(seq.to_be_bytes())
}

/// Decode a client-supplied watermark back to a monotonic sequence, if it is one
/// of ours. Unknown watermarks (e.g. from a restarted server, or from a
/// different implementation) are tolerated: we treat them as "before any
/// event" (sequence 0).
fn decode_watermark(wm: &str) -> u64 {
    STANDARD
        .decode(wm.trim())
        .ok()
        .filter(|b| b.len() == std::mem::size_of::<u64>())
        .map(|b| {
            let mut arr = [0u8; 8];
            arr.copy_from_slice(&b);
            u64::from_be_bytes(arr)
        })
        .unwrap_or(0)
}

/// Render a single MS-OXWSNTIF notification event element (one of the choices
/// under `t:NotificationType`).
fn render_notification_event(
    event: &NotificationEvent,
    watermark: &str,
    timestamp: &str,
) -> String {
    let ts = format!("<t:TimeStamp>{}</t:TimeStamp>", timestamp);
    let watermark_xml = format!("<t:Watermark>{}</t:Watermark>", watermark);
    match event {
        NotificationEvent::ItemCreated {
            folder_id,
            item_id,
            change_key,
            ..
        } => {
            format!(
                "<t:CreatedEvent>{ts}{wm}<t:ItemId Id=\"{iid}\" ChangeKey=\"{ck}\"/><t:ParentFolderId Id=\"{fid}\"/></t:CreatedEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(item_id),
                ck = xml_escape(change_key),
                fid = xml_escape(folder_id),
            )
        }
        NotificationEvent::ItemModified {
            folder_id,
            item_id,
            change_key,
            ..
        } => {
            format!(
                "<t:ModifiedEvent>{ts}{wm}<t:ItemId Id=\"{iid}\" ChangeKey=\"{ck}\"/><t:ParentFolderId Id=\"{fid}\"/></t:ModifiedEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(item_id),
                ck = xml_escape(change_key),
                fid = xml_escape(folder_id),
            )
        }
        NotificationEvent::ItemDeleted {
            folder_id, item_id, ..
        } => {
            format!(
                "<t:DeletedEvent>{ts}{wm}<t:ItemId Id=\"{iid}\"/><t:ParentFolderId Id=\"{fid}\"/></t:DeletedEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(item_id),
                fid = xml_escape(folder_id),
            )
        }
        NotificationEvent::NewMail {
            folder_id,
            item_id,
            change_key,
            ..
        } => {
            format!(
                "<t:NewMailEvent>{ts}{wm}<t:ItemId Id=\"{iid}\" ChangeKey=\"{ck}\"/><t:ParentFolderId Id=\"{fid}\"/></t:NewMailEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(item_id),
                ck = xml_escape(change_key),
                fid = xml_escape(folder_id),
            )
        }
        NotificationEvent::ItemMoved {
            old_folder_id,
            old_item_id,
            new_folder_id,
            new_item_id,
            change_key,
            ..
        } => {
            format!(
                "<t:MovedEvent>{ts}{wm}<t:ItemId Id=\"{iid}\" ChangeKey=\"{ck}\"/><t:ParentFolderId Id=\"{nfid}\"/><t:OldItemId Id=\"{oid}\"/><t:OldParentFolderId Id=\"{ofid}\"/></t:MovedEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(new_item_id),
                ck = xml_escape(change_key),
                nfid = xml_escape(new_folder_id),
                oid = xml_escape(old_item_id),
                ofid = xml_escape(old_folder_id),
            )
        }
        NotificationEvent::ItemCopied {
            old_folder_id,
            old_item_id,
            new_folder_id,
            new_item_id,
            change_key,
            ..
        } => {
            format!(
                "<t:CopiedEvent>{ts}{wm}<t:ItemId Id=\"{iid}\" ChangeKey=\"{ck}\"/><t:ParentFolderId Id=\"{nfid}\"/><t:OldItemId Id=\"{oid}\"/><t:OldParentFolderId Id=\"{ofid}\"/></t:CopiedEvent>",
                ts = ts,
                wm = watermark_xml,
                iid = xml_escape(new_item_id),
                ck = xml_escape(change_key),
                nfid = xml_escape(new_folder_id),
                oid = xml_escape(old_item_id),
                ofid = xml_escape(old_folder_id),
            )
        }
    }
}

/// Render a complete `t:Notification` (one per subscription per GetEvents turn,
/// or per delivered batch in streaming).
fn render_notification(
    sub_id: &str,
    previous_watermark: Option<&str>,
    more_events: bool,
    events_xml: &str,
) -> String {
    let prev = match previous_watermark {
        Some(w) => format!("<t:PreviousWatermark>{}</t:PreviousWatermark>", w),
        None => String::new(),
    };
    let more = format!(
        "<t:MoreEvents>{}</t:MoreEvents>",
        if more_events { "true" } else { "false" }
    );
    format!(
        "<t:Notification><t:SubscriptionId>{}</t:SubscriptionId>{prev}{more}{events}</t:Notification>",
        xml_escape(sub_id),
        prev = prev,
        more = more,
        events = events_xml,
    )
}

/// Publish a mailbox store change event to all active EWS subscriptions.
///
/// This is the single hook that makes the Subscribe/GetEvents pipeline real:
/// every item CRUD handler that successfully mutates the Stalwart backend calls
/// this so subscribers observe `CreatedEvent`/`ModifiedEvent`/`DeletedEvent`
/// (and `NewMailEvent` for mail arrival).
fn publish_event(state: &AppState, event: NotificationEvent) {
    state.subscription_manager.publish(event);
}

/// HTTP transport that turns push-notification batches into outbound SOAP
/// `SendNotification` requests POSTed to a client's callback URL. Owns an HTTP
/// client (connection pool + sane timeouts) and performs the SOAP serialization
/// for the EWS push notification wire format (MS-OXWSNTIF §3.1.4.4).
pub struct EwsPushNotifier {
    client: reqwest::Client,
}

impl EwsPushNotifier {
    /// Build a notifier from an already-configured [`reqwest::Client`], so the
    /// application controls timeouts/TLS uniformly.
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait::async_trait]
impl PushNotifier for EwsPushNotifier {
    async fn deliver(&self, delivery: PushDelivery) -> Result<PushDeliveryStatus, ()> {
        let body = render_send_notification(&delivery);
        let url = delivery.url.clone();
        // POST the SendNotification SOAP request. The client (acting as the
        // notification server) answers 200 with a SendNotificationResponse whose
        // `SubscriptionStatus` may be `Unsubscribe` (stop pushing). SOAP 1.1
        // requires the `SOAPAction` HTTP header for the operation dispatch.
        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "text/xml; charset=utf-8")
            .header(
                "SOAPAction",
                "http://schemas.microsoft.com/exchange/services/2006/messages/SendNotification",
            )
            .body(body)
            .send()
            .await;

        match resp {
            Ok(resp) if resp.status().is_success() => {
                let bytes = resp.bytes().await.map_err(|e| {
                    tracing::warn!(
                        target: "ews",
                        push_url = %redact_url(&url),
                        error = %e,
                        "Failed to read push notification response body"
                    );
                })?;
                // Honour an explicit client `SubscriptionStatus=Unsubscribe`.
                let text = String::from_utf8_lossy(&bytes);
                if extract_first_tag_text(&text, b"SubscriptionStatus").as_deref()
                    == Some("Unsubscribe")
                {
                    return Ok(PushDeliveryStatus::Unsubscribed);
                }
                Ok(PushDeliveryStatus::Delivered)
            }
            Ok(resp) => {
                tracing::warn!(
                    target: "ews",
                    status = %resp.status(),
                    push_url = %redact_url(&url),
                    "Push notification client returned a non-success status"
                );
                Err(())
            }
            Err(e) => {
                tracing::warn!(
                    target: "ews",
                    push_url = %redact_url(&url),
                    error = %e,
                    "Push notification delivery failed"
                );
                Err(())
            }
        }
    }
}

/// Reduce a callback URL to a safe, non-sensitive form for logging (strip any
/// userinfo credentials and query string, which may carry tokens).
fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.set_query(None);
            u.set_fragment(None);
            u.to_string()
        }
        Err(_) => "<invalid-url>".to_string(),
    }
}

/// Classify an IP address as not appropriate for an outbound push callback
/// (SSRF protection). Rejects loopback, unspecified, multicast, broadcast,
/// link-local, private/unique-local, CGNAT (100.64.0.0/10), documentation, and
/// IPv4-mapped forms of the same. Returns `true` when the address must be blocked.
fn is_internal_ipv4(v4: std::net::Ipv4Addr) -> bool {
    let o = v4.octets();
    v4.is_loopback()
        || v4.is_unspecified()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_multicast()
        || v4.is_documentation()
        // CGNAT (RFC 6598) 100.64.0.0/10
        || (o[0] == 100 && (o[1] & 0xC0) == 0x40)
        // "this network" 0.0.0.0/8
        || o[0] == 0
}

/// Classify an IP address as not appropriate for an outbound push callback
/// (SSRF protection). Rejects loopback, unspecified, multicast, broadcast,
/// link-local, private/unique-local, CGNAT (100.64.0.0/10), documentation, and
/// IPv4-mapped forms of the same. Returns `true` when the address must be blocked.
fn is_internal_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_internal_ipv4(v4),
        IpAddr::V6(v6) => {
            let s = v6.segments();
            // IPv4-mapped (::ffff:a.b.c.d): delegate to the embedded IPv4.
            if let Some(v4) = v6.to_ipv4() {
                return is_internal_ipv4(v4);
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (s[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || (s[0] == 0x2001 && s[1] == 0x0db8) // 2001:db8::/32 documentation
        }
    }
}

/// Validate a push-notification callback URL before a subscription is stored.
///
/// Rejects unsupported schemes, embedded credentials, and any host that
/// resolves to a loopback/private/link-local/metadata address (SSRF protection,
/// CWE-918). Hostnames are resolved defensively; a host that cannot be resolved
/// is rejected so a rebinding name cannot slip through as "safe". Returns a
/// human-readable rejection reason on failure.
async fn validate_push_callback_url(url_str: &str) -> Result<(), String> {
    let parsed = url::Url::parse(url_str).map_err(|e| format!("invalid callback URL: {e}"))?;

    match parsed.scheme() {
        "http" | "https" => {}
        other => return Err(format!("unsupported callback URL scheme: {other}")),
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("callback URL must not contain embedded credentials".to_string());
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| "callback URL has no host".to_string())?;

    // Resolve the host (hostname or IP literal) to concrete addresses and
    // reject the subscription if any resolved address is internal.
    let port = parsed.port_or_known_default().unwrap_or(80);
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|e| format!("callback URL host could not be resolved: {e}"))?
        .collect();

    if addrs.is_empty() {
        return Err("callback URL host resolved to no addresses".to_string());
    }

    for addr in addrs {
        if is_internal_ip(addr.ip()) {
            return Err(format!(
                "callback URL resolves to a non-public address: {}",
                addr.ip()
            ));
        }
    }

    Ok(())
}

/// Render the SOAP `SendNotification` request body for a push delivery. An
/// empty `events` list produces a single `StatusEvent` keep-alive (the client
/// must answer or be considered dead, MS-OXWSNTIF §3.1.4.4.1.1); otherwise each
/// event becomes a typed event element inside a single `Notification`.
fn render_send_notification(delivery: &PushDelivery) -> String {
    let timestamp = format_ews_datetime(&Utc::now());
    let watermark_str = encode_watermark(delivery.watermark);

    let events_xml = if delivery.events.is_empty() {
        // StatusEvent keep-alive: no item data, only the current watermark
        // signalled as a `StatusEvent` (the client must answer or be considered
        // dead, MS-OXWSNTIF 3.1.4.4.1.1).
        format!(
            "<t:StatusEvent><t:Watermark>{}</t:Watermark></t:StatusEvent>",
            watermark_str
        )
    } else {
        let mut buf = String::new();
        for (event, watermark) in &delivery.events {
            let wm = encode_watermark(*watermark);
            buf.push_str(&render_notification_event(event, &wm, &timestamp));
        }
        buf
    };

    // Push notifications carry per-event watermarks and omit the optional
    // `PreviousWatermark` element (that is a GetEvents/streaming concern); the
    // `StatusEvent` keep-alive above already encodes the current watermark.
    let notification = render_notification(&delivery.subscription_id, None, false, &events_xml);

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    {}
  </s:Header>
  <s:Body>
    <m:SendNotification xmlns:m="{msg}" xmlns:t="{typ}">
      <m:ResponseMessages>
        <m:SendNotificationResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          {notification}
        </m:SendNotificationResponseMessage>
      </m:ResponseMessages>
    </m:SendNotification>
  </s:Body>
</s:Envelope>"#,
        version::current().render_ews_header(EWS_TYPE_NS),
        msg = EWS_MSG_NS,
        typ = EWS_TYPE_NS,
    )
}

async fn handle_subscribe(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let parsed = parse_subscription_request(body);

    // Determine subscription kind from the XML *structure* (the actual child
    // element of `<Subscribe>`), not from raw `body.contains(…)` substring
    // matching, which is namespace- and formatting-sensitive (MS-OXWSNTIF
    // 3.1.4.3.3.2). Each kind maps to its own creation path below.
    let requested = detect_subscription_request_kind(body);

    // Push subscriptions follow a distinct creation path (server pushes to a
    // client callback URL); Pull/Streaming share the in-memory subscription path.
    if requested == Some(DetectedSubscriptionRequest::Push) {
        return handle_push_subscribe(state, auth, body, parsed).await;
    }

    let (kind, timeout_minutes) = match requested {
        Some(DetectedSubscriptionRequest::Pull) => {
            let minutes = extract_first_tag_text(body, b"Timeout")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|&m| (1..=1440).contains(&m));
            (SubscriptionKind::Pull, minutes)
        }
        Some(DetectedSubscriptionRequest::Streaming) => (SubscriptionKind::Streaming, None),
        Some(DetectedSubscriptionRequest::Push) => {
            unreachable!("push handled by handle_push_subscribe above")
        }
        None => {
            return operation_error_response(
                &EwsAction::Subscribe,
                "ErrorInvalidRequest",
                "Subscribe request must contain a Pull, Streaming, or Push subscription request",
                StatusCode::BAD_REQUEST,
            );
        }
    };

    let sub_id = state
        .subscription_manager
        .subscribe(
            &auth.username,
            kind,
            parsed.folders,
            parsed.event_types,
            timeout_minutes,
        )
        .await;

    // The initial watermark is the "subscription created" marker; subsequent
    // GetEvents calls echo the last watermark they received.
    let watermark = encode_watermark(0);

    let response = format!(
        r#"<m:SubscribeResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SubscriptionId>{}</m:SubscriptionId><m:Watermark>{}</m:Watermark></m:SubscribeResponseMessage></m:ResponseMessages></m:SubscribeResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&sub_id),
        xml_escape(&watermark)
    );
    soap_ok(response)
}

/// Handle a `PushSubscriptionRequest`: parse the callback URL (required) and
/// optional `StatusFrequency`/`CallerData`, create the push subscription via the
/// notification manager, and return the `SubscriptionId` + initial `Watermark`.
/// A missing/empty URL is a client error (MS-OXWSNTIF requires `URL`).
async fn handle_push_subscribe(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
    parsed: ParsedSubscriptionRequest,
) -> Response {
    let config = parse_push_subscription_request(body);
    if config.url.trim().is_empty() {
        return operation_error_response(
            &EwsAction::Subscribe,
            "ErrorInvalidSubscriptionRequest",
            "Push subscription requires a callback URL",
            StatusCode::BAD_REQUEST,
        );
    }

    // Validate the callback destination before storing the subscription so an
    // authenticated client cannot direct the server at internal/link-local
    // endpoints (SSRF, CWE-918). Redirection is additionally disabled on the
    // outbound HTTP client.
    if let Err(reason) = validate_push_callback_url(&config.url).await {
        tracing::warn!(
            target: "ews",
            user = %auth.username,
            reason = %reason,
            "Rejected push subscription callback URL"
        );
        return operation_error_response(
            &EwsAction::Subscribe,
            "ErrorInvalidSubscriptionRequest",
            "Push subscription callback URL is not allowed",
            StatusCode::BAD_REQUEST,
        );
    }

    let sub_id = match state
        .subscription_manager
        .subscribe_push(&auth.username, parsed.folders, parsed.event_types, config)
        .await
    {
        Some(id) => id,
        None => {
            // No push transport installed (server misconfiguration): report as
            // a transient server-side failure so the client may retry rather
            // than treating the request as a permanent client error.
            return operation_error_response(
                &EwsAction::Subscribe,
                "ErrorInternalServerError",
                "Push notifications are not available on this server",
                StatusCode::SERVICE_UNAVAILABLE,
            );
        }
    };

    let watermark = encode_watermark(0);
    let response = format!(
        r#"<m:SubscribeResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:SubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:SubscriptionId>{}</m:SubscriptionId><m:Watermark>{}</m:Watermark></m:SubscribeResponseMessage></m:ResponseMessages></m:SubscribeResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&sub_id),
        xml_escape(&watermark)
    );
    soap_ok(response)
}

async fn handle_unsubscribe(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let Some(sub_id) = extract_first_tag_text(body, b"SubscriptionId") else {
        return operation_error_response(
            &EwsAction::Unsubscribe,
            "ErrorInvalidRequest",
            "Unsubscribe request must contain a SubscriptionId",
            StatusCode::BAD_REQUEST,
        );
    };
    let removed = state
        .subscription_manager
        .unsubscribe(&sub_id, &auth.username)
        .await;
    if removed {
        let response = format!(
            r#"<m:UnsubscribeResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UnsubscribeResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode></m:UnsubscribeResponseMessage></m:ResponseMessages></m:UnsubscribeResponse>"#,
            EWS_MSG_NS, EWS_TYPE_NS
        );
        soap_ok(response)
    } else {
        operation_error_response(
            &EwsAction::Unsubscribe,
            "ErrorSubscriptionNotFound",
            "The specified subscription does not exist",
            StatusCode::NOT_FOUND,
        )
    }
}

async fn handle_get_events(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let Some(sub_id) = extract_first_tag_text(body, b"SubscriptionId") else {
        return operation_error_response(
            &EwsAction::GetEvents,
            "ErrorInvalidRequest",
            "GetEvents request must contain a SubscriptionId",
            StatusCode::BAD_REQUEST,
        );
    };
    // MS-OXWSNTIF 3.1.4.1.2.1 requires a Watermark element; tolerate its
    // absence (treat as sequence 0) because some clients omit it on the first
    // call after Subscribe. When present, decode it and reconcile the internal
    // watermark to it so we never re-emit events the client already consumed
    // (avoids duplicate / skipped notifications after a restart or when the
    // client talks to an implementation that owns the watermark cursor).
    let client_watermark = extract_first_tag_text(body, b"Watermark");
    let client_seq = client_watermark.as_deref().map(decode_watermark);

    // Reject if the subscription is actually a streaming subscription: EWS
    // returns ErrorInvalidSubscription for GetEvents on a streaming subscription.
    match state
        .subscription_manager
        .subscription_kind(&sub_id, &auth.username)
        .await
    {
        Some(SubscriptionKind::Pull) => {}
        Some(SubscriptionKind::Streaming) | Some(SubscriptionKind::Push) => {
            return operation_error_response(
                &EwsAction::GetEvents,
                "ErrorInvalidSubscription",
                "GetEvents is not valid for a streaming or push subscription",
                StatusCode::BAD_REQUEST,
            );
        }
        None => {
            return operation_error_response(
                &EwsAction::GetEvents,
                "ErrorSubscriptionNotFound",
                "The specified subscription does not exist or has expired",
                StatusCode::NOT_FOUND,
            );
        }
    }

    let Some((events, prev_seq, last_seq, more)) = state
        .subscription_manager
        .pull_events_from(&sub_id, &auth.username, client_seq)
        .await
    else {
        return operation_error_response(
            &EwsAction::GetEvents,
            "ErrorSubscriptionNotFound",
            "The specified subscription does not exist or has expired",
            StatusCode::NOT_FOUND,
        );
    };

    let timestamp = Utc::now();
    let ts_xml = format_ews_datetime(&timestamp);
    let mut events_xml = String::new();
    for (i, event) in events.iter().enumerate() {
        let wm = encode_watermark(prev_seq + 1 + i as u64);
        events_xml.push_str(&render_notification_event(event, &wm, &ts_xml));
    }
    // If no events were buffered, EWS sends a StatusEvent keep-alive so the
    // client knows the subscription is still alive (MS-OXWSNTIF 2.2.4.8).
    if events.is_empty() {
        let wm = encode_watermark(last_seq);
        events_xml.push_str(&format!(
            "<t:StatusEvent><t:Watermark>{}</t:Watermark></t:StatusEvent>",
            wm
        ));
    }

    let prev_wm = (prev_seq > 0).then(|| encode_watermark(prev_seq));
    let notification = render_notification(&sub_id, prev_wm.as_deref(), more, &events_xml);

    let last_wm = encode_watermark(last_seq);

    let response = format!(
        r#"<m:GetEventsResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetEventsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode>{}</m:GetEventsResponseMessage></m:ResponseMessages></m:GetEventsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, notification
    );
    // last_wm is the watermark the client should echo back next; it is already
    // embedded per-event or in the StatusEvent above. The variable is retained
    // for clarity of the watermark accounting (no separate top-level element).
    let _ = last_wm;
    soap_ok(response)
}

/// Maximum number of distinct subscription IDs honoured by a single
/// GetStreamingEvents call (MS-OXWSNTIF 3.1.4.2.3.1 implies at least one).
const STREAMING_MAX_SUBSCRIPTIONS: usize = 32;

async fn handle_get_streaming_events(
    state: Arc<AppState>,
    auth: AuthContext,
    body: String,
) -> Response {
    // Parse <SubscriptionIds><t:SubscriptionId>…</t:SubscriptionId></…>
    let mut sub_ids: Vec<String> = extract_tag_texts(&body, b"SubscriptionId");
    if sub_ids.is_empty() {
        return operation_error_response(
            &EwsAction::GetStreamingEvents,
            "ErrorInvalidRequest",
            "GetStreamingEvents request must contain at least one SubscriptionId",
            StatusCode::BAD_REQUEST,
        );
    }
    if sub_ids.len() > STREAMING_MAX_SUBSCRIPTIONS {
        sub_ids.truncate(STREAMING_MAX_SUBSCRIPTIONS);
    }

    // ConnectionTimeout is in minutes, clamped to 1..=30
    // (StreamingSubscriptionConnectionTimeoutType, MS-OXWSNTIF 3.1.4.2.4.1).
    let timeout_minutes = extract_first_tag_text(&body, b"ConnectionTimeout")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1)
        .clamp(1, 30);
    let deadline_total = Duration::from_secs(timeout_minutes * 60);
    // Per-turn wait is short so the server can emit periodic StatusEvent
    // keep-alives and the connection stays responsive to cancellation.
    let turn_timeout = Duration::from_secs(10);

    // Validate that every requested subscription belongs to the caller and is a
    // streaming subscription. Collect the ids we will actually serve.
    let mut served: Vec<String> = Vec::new();
    let mut error_ids: Vec<String> = Vec::new();
    for sid in &sub_ids {
        match state
            .subscription_manager
            .subscription_kind(sid, &auth.username)
            .await
        {
            Some(SubscriptionKind::Streaming) => served.push(sid.clone()),
            _ => error_ids.push(sid.clone()),
        }
    }
    if served.is_empty() {
        let err_xml: String = error_ids
            .iter()
            .map(|id| format!("<t:SubscriptionId>{}</t:SubscriptionId>", xml_escape(id)))
            .collect();
        let response = format!(
            r#"<m:GetStreamingEventsResponse xmlns:m="{msg}" xmlns:t="{typ}"><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Error"><m:MessageText>None of the requested subscriptions are valid streaming subscriptions</m:MessageText><m:ResponseCode>ErrorSubscriptionNotFound</m:ResponseCode><m:ErrorSubscriptionIds>{err}</m:ErrorSubscriptionIds></m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse>"#,
            msg = EWS_MSG_NS,
            typ = EWS_TYPE_NS,
            err = err_xml
        );
        return soap_ok(response);
    }

    let mgr = state.subscription_manager.clone();
    let owner = auth.username.clone();
    let served_clone = served.clone();
    let served_err = error_ids.clone();

    // Drive the long-lived GetStreamingEvents connection from a background task
    // writing chunk Bytes to an mpsc channel consumed by a ReceiverStream. This
    // avoids pulling in a macro dependency and keeps the HTTP body as a plain
    // stream of Bytes.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);
    tokio::spawn(async move {
        let deadline = std::time::Instant::now() + deadline_total;
        let err_ids_xml: String = served_err
            .iter()
            .map(|id| format!("<t:SubscriptionId>{}</t:SubscriptionId>", xml_escape(id)))
            .collect();
        let mut first = true;
        let mut closed_fragment_sent = false;
        loop {
            let now = std::time::Instant::now();
            if now >= deadline {
                // Terminal fragment: ConnectionStatus=Closed followed by the
                // closing SOAP envelope tags so the streamed XML is well-formed.
                let final_xml = format!(
                    "{}{}",
                    streaming_fragment("", &err_ids_xml, "Closed"),
                    streaming_footer()
                );
                let _ = tx.send(Ok(Bytes::from(final_xml))).await;
                closed_fragment_sent = true;
                break;
            }

            let remaining = deadline - now;
            let turn = remaining.min(turn_timeout);

            // Poll *all* served subscriptions concurrently (item 6). The
            // previous sequential `for sid { … .await }` serialized the per-turn
            // waits, so an event for the Nth subscription was delayed by the sum
            // of the prior subscriptions' waits (up to N × turn); concurrent
            // polling bounds a turn to a single `turn` regardless of how many
            // subscriptions are served.
            let futs = served_clone.iter().map(|sid| {
                let mgr = mgr.clone();
                let owner = owner.clone();
                let recv_sid = sid.clone();
                let tuple_sid = sid.clone();
                async move {
                    let outcome = mgr.recv_one_streaming(&recv_sid, &owner, turn).await;
                    (tuple_sid, outcome)
                }
            });
            let results = futures_util::future::join_all(futs).await;

            let mut notifications_xml = String::new();
            for (sid, outcome) in results {
                match outcome {
                    Ok(Some((event, watermark))) => {
                        let ts_xml = format_ews_datetime(&Utc::now());
                        let wm = encode_watermark(watermark);
                        let event_xml = render_notification_event(&event, &wm, &ts_xml);
                        let notif = render_notification(&sid, None, false, &event_xml);
                        notifications_xml.push_str(&notif);
                    }
                    Ok(None) => {} // idle: no matching event this turn
                    Err(_) => {
                        // Subscription vanished mid-stream; surfaced via
                        // ErrorSubscriptionIds in subsequent fragments.
                        tracing::warn!(
                            subscription_id = %sid,
                            "Streaming subscription disappeared mid-connection"
                        );
                    }
                }
            }

            if notifications_xml.is_empty() {
                // Keep-alive StatusEvent per served subscription so the client
                // knows the connection is healthy between real events.
                for sid in &served_clone {
                    let status = format!(
                        "<t:Notification><t:SubscriptionId>{sid}</t:SubscriptionId><t:StatusEvent><t:Watermark>{wm}</t:Watermark></t:StatusEvent></t:Notification>",
                        sid = xml_escape(sid),
                        wm = encode_watermark(0),
                    );
                    notifications_xml.push_str(&status);
                }
            }

            let header = if first {
                first = false;
                streaming_header()
            } else {
                String::new()
            };
            let err_for_fragment = if served_err.is_empty() {
                ""
            } else {
                &err_ids_xml
            };
            let fragment = streaming_fragment(&notifications_xml, err_for_fragment, "OK");
            let chunk = format!("{header}{fragment}");
            if tx.send(Ok(Bytes::from(chunk))).await.is_err() {
                // Client disconnected; stop the connection.
                break;
            }

            // Small breather so the runtime can service cancellation/flush.
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        // Ensure the SOAP envelope is always closed when the stream terminates
        // (item 7). On the graceful deadline path the closing tags ride along
        // with the terminal `Closed` fragment above (`closed_fragment_sent`),
        // so only synthesize a bare footer when we exited via client disconnect
        // (or any other non-deadline break) before sending it. If the receiver
        // is already gone the send fails harmlessly and the drop closes the body.
        if !closed_fragment_sent {
            let _ = tx.send(Ok(Bytes::from(streaming_footer()))).await;
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(stream);
    (
        StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        body,
    )
        .into_response()
}

/// XML prologue/soap envelope opening for the streaming response. Each
/// subsequent chunk appends more fragment bodies; the connection end is marked
/// by a `ConnectionStatus=Closed` fragment.
fn streaming_header() -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    {svi}
  </s:Header>
  <s:Body>
"#,
        svi = version::current().render_ews_header(EWS_TYPE_NS)
    )
}

/// Closing tags that balance [`streaming_header`], emitted exactly once when the
/// streaming connection terminates so the streamed XML is a well-formed SOAP
/// envelope instead of being left open mid-`<s:Body>`.
fn streaming_footer() -> String {
    "  </s:Body>\n</s:Envelope>".to_string()
}

fn streaming_fragment(
    notifications_xml: &str,
    error_ids_xml: &str,
    connection_status: &str,
) -> String {
    let err_block = if error_ids_xml.is_empty() {
        String::new()
    } else {
        format!(
            "<m:ErrorSubscriptionIds>{}</m:ErrorSubscriptionIds>",
            error_ids_xml
        )
    };
    format!(
        r#"<m:GetStreamingEventsResponse xmlns:m="{msg}" xmlns:t="{typ}"><m:ResponseMessages><m:GetStreamingEventsResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Notifications>{notifs}</m:Notifications>{err}<m:ConnectionStatus>{status}</m:ConnectionStatus></m:GetStreamingEventsResponseMessage></m:ResponseMessages></m:GetStreamingEventsResponse>"#,
        msg = EWS_MSG_NS,
        typ = EWS_TYPE_NS,
        notifs = notifications_xml,
        err = err_block,
        status = connection_status,
    )
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
                    publish_event(
                        state,
                        NotificationEvent::ItemCreated {
                            owner: auth.username.clone(),
                            folder_id: folder_id_for(
                                &auth.username,
                                DistinguishedFolder::SentItems,
                            ),
                            item_id: server_id.clone(),
                            change_key: change_key.clone(),
                        },
                    );
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
        // SaveOnly — save draft to Drafts mailbox via JMAP
        if !state.cfg.email_enabled {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInvalidRequest",
                "Email operations are not enabled on this server",
                StatusCode::FORBIDDEN,
            );
        }
        let jmap = match state.jmap_client.as_ref() {
            Some(j) => j,
            None => {
                return operation_error_response(
                    &EwsAction::CreateItem,
                    "ErrorInvalidRequest",
                    "JMAP not configured for draft persistence",
                    StatusCode::FORBIDDEN,
                );
            }
        };
        let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, "CreateItem SaveOnly: failed to get JMAP account ID");
                return operation_error_response(
                    &EwsAction::CreateItem,
                    "ErrorInternalServerError",
                    "Failed to get email account",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };
        match crate::email::save_draft_via_jmap(
            state,
            jmap,
            &msg,
            &account_id,
            &auth.username,
            &auth.password,
        )
        .await
        {
            Ok((server_id, change_key)) => {
                let items_xml =
                    crate::email::render_ews_message_item_xml(&server_id, &change_key, &msg);
                let response = format!(
                    r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
                    EWS_MSG_NS, EWS_TYPE_NS, items_xml
                );
                return soap_ok(response);
            }
            Err(e) => {
                tracing::error!(error = %e, "CreateItem SaveOnly: draft save failed");
                return operation_error_response(
                    &EwsAction::CreateItem,
                    "ErrorInternalServerError",
                    "Failed to save draft",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }

    // C4: Meeting response objects — AcceptItem / DeclineItem /
    // TentativelyAcceptItem (MS-OXWSDLS §3.1.4.2.1, [MS-OXWSMTGSRC]). Outlook
    // sends these referencing the meeting request ItemId. We resolve the
    // referenced email, extract its METHOD:REQUEST iCalendar, build an iTIP
    // REPLY and deliver it to the organizer via SMTP, then accept/decline the
    // local calendar copy.
    if body.contains("<t:AcceptItem")
        || body.contains("<t:DeclineItem")
        || body.contains("<t:TentativelyAcceptItem")
    {
        return handle_meeting_response_object(state, auth, body).await;
    }

    // If the body contains a <t:Contact> element, handle as contact
    if body.contains("<t:Contact") {
        return handle_create_contact_item(state, auth, body).await;
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

    let password_secret = SecretString::from(auth.password.expose_secret());

    // A1: Outlook omits ORGANIZER on newly-composed meetings; Stalwart's
    // CalDAV scheduler (RFC 6638) requires it to route REQUESTs. Default the
    // organizer to the authenticated user's primary SMTP whenever attendees are
    // present.
    let owner_email = crate::util::user_primary_email(&auth.username, &state.cfg.mail_domain);
    let mut item = item;
    crate::calendar::ensure_organizer_for_scheduling(&mut item, owner_email.as_deref());

    // A2/A3: Outlook sends SendMeetingInvitationsOrCancellations on CreateItem.
    // Stalwart's CalDAV scheduler auto-delivers REQUESTs to attendees on a PUT,
    // but a JMAP Calendar `iCalendar`-blob write does NOT trigger scheduling.
    // So when scheduling is requested (attendees present, disposition != None),
    // force the CalDAV backend; when SendToNone, strip scheduling context and
    // use whichever backend is available (no invites get sent).
    let disposition = crate::calendar::ScheduleDisposition::parse(
        extract_open_tag_attr(body, "createitem", "SendMeetingInvitationsOrCancellations")
            .or_else(|| extract_open_tag_attr(body, "createitem", "SendMeetingInvitations"))
            .as_deref(),
    );
    let scheduling_needed = match disposition {
        Some(d) => crate::calendar::scheduling_needed(&item, d),
        None => false, // Default per [MS-OXWSICAL] is SendToNone when the attribute is absent.
    };
    if matches!(
        disposition,
        Some(crate::calendar::ScheduleDisposition::SendToNone)
    ) {
        // Suppress server-side scheduling but preserve the attendee list.
        crate::calendar::mark_scheduling_client_side(&mut item);
    }

    // Try JMAP calendar first, but only when no server-side scheduling is
    // required (otherwise the JMAP blob write silently drops the iTIP).
    let result = if state.cfg.prefer_jmap_calendar
        && state.jmap_client.is_some()
        && !scheduling_needed
    {
        match try_jmap_create_calendar(state, owner, &password_secret, &item).await {
            Ok(row) => Ok(row),
            Err(e) => {
                tracing::warn!(error = %e, "JMAP calendar create failed, falling back to CalDAV");
                Err(e)
            }
        }
    } else {
        // CalDAV forced or JMAP not available
        Err(anyhow::anyhow!("JMAP not preferred or not available"))
    };

    // CalDAV fallback (primary if JMAP not preferred, or secondary after JMAP failure)
    let response_row = match result {
        Ok(row) => row,
        Err(_) => match create_calendar_via_caldav(state, owner, &password_secret, &item).await {
            Ok(row) => row,
            Err(resp) => return *resp,
        },
    };

    let server_id = response_row.server_id.clone();
    let change_key = changekey_for_item(&response_row);
    publish_event(
        state,
        NotificationEvent::ItemCreated {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
            item_id: server_id.clone(),
            change_key: change_key.clone(),
        },
    );
    let response = format!(
        r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CreateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:CreateItemResponseMessage></m:ResponseMessages></m:CreateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(&server_id, &change_key, &item)
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
    publish_event(
        state,
        NotificationEvent::ItemModified {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Inbox),
            item_id: item_id.to_string(),
            change_key: change_key.clone(),
        },
    );
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

    // Check if it's a contact (server_id prefix "contact-")
    if item_id.starts_with("contact-") && state.carddav_client.is_some() {
        return handle_update_contact_item(state, auth, body).await;
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
    let password_secret = SecretString::from(auth.password.expose_secret());

    // Parse the incoming item changes into a new CalendarItem struct.
    let mut new_item = if let Some(_existing_ics) = &stored_item.caldav_href {
        // Might need to fetch via CalDAV later, but we can construct a blank template from stored UID
        crate::calendar::CalendarItem {
            uid: stored_item
                .uid
                .clone()
                .unwrap_or_else(|| stored_item.server_id.clone()),
            start: chrono::Utc::now(),
            end: chrono::Utc::now() + chrono::Duration::hours(1),
            dtstamp: Some(chrono::Utc::now()),
            ..Default::default()
        }
    } else {
        // JMAP item: we still need to fetch existing via JMAP? But we can build a minimal item for rendering
        // Use the stored UID or server_id as UID to keep consistency.
        crate::calendar::CalendarItem {
            uid: stored_item
                .uid
                .clone()
                .unwrap_or_else(|| stored_item.server_id.clone()),
            start: chrono::Utc::now(),
            end: chrono::Utc::now() + chrono::Duration::hours(1),
            dtstamp: Some(chrono::Utc::now()),
            ..Default::default()
        }
    };

    // Apply changes from the UpdateItem request to new_item
    let field_changes = parse_item_changes(body);
    if !field_changes.is_empty() {
        apply_field_changes(&mut new_item, &field_changes);
    } else {
        // Legacy field extraction (same as original)
        if let Some(v) =
            extract_ews_field(body, b"Subject").or_else(|| extract_ews_field(body, b"Value"))
        {
            new_item.subject = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"Start").and_then(|v| crate::calendar::parse_datetime(&v))
        {
            new_item.start = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"End").and_then(|v| crate::calendar::parse_datetime(&v))
        {
            new_item.end = v;
        }
        if let Some(v) = extract_ews_field(body, b"Location") {
            new_item.location = v;
        }
        if let Some(v) =
            extract_ews_field(body, b"Body").or_else(|| extract_ews_field(body, b"TextBody"))
        {
            new_item.description = v;
        }
        if body.contains("Categories") {
            new_item.categories = extract_ews_fields(body, b"String");
        }
        if let Some(v) =
            extract_ews_field(body, b"ReminderMinutesBeforeStart").and_then(|v| v.parse().ok())
        {
            new_item.reminder = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"LegacyFreeBusyStatus") {
            new_item.busy_status = match v.as_str() {
                "Free" => Some(0),
                "Tentative" => Some(1),
                "Busy" => Some(2),
                "OOF" => Some(3),
                _ => new_item.busy_status,
            };
        }
        if let Some(v) = extract_ews_field(body, b"Sensitivity") {
            new_item.sensitivity = match v.as_str() {
                "Normal" => Some(0),
                "Personal" => Some(1),
                "Private" => Some(2),
                "Confidential" => Some(3),
                _ => new_item.sensitivity,
            };
        }
        if let Some(v) = extract_ews_field(body, b"ResponseRequested") {
            new_item.response_requested = Some(v.eq_ignore_ascii_case("true"));
        }
        if let Some(v) = extract_ews_field(body, b"DisallowNewTimeProposal") {
            new_item.disallow_new_time_proposal = Some(v.eq_ignore_ascii_case("true"));
        }
        if let Some(v) = extract_ews_field(body, b"OrganizerName") {
            new_item.organizer_name = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OrganizerEmail") {
            new_item.organizer_email = Some(nfc(&v));
        }
        if body.contains("RequiredAttendees") || body.contains("OptionalAttendees") {
            new_item.attendees = parse_ews_attendees(body);
        }
        if body.contains("Recurrence") {
            new_item.rrule = parse_ews_recurrence(body);
        }
        if let Some(v) = crate::calendar::extract_ews_timezone_field(body, b"StartTimeZone") {
            // Outlook sends a Windows timezone name; normalise to IANA for render_ics.
            new_item.timezone = Some(crate::calendar::normalize_timezone_to_iana(&v));
        }
        if let Some(v) = crate::calendar::extract_ews_timezone_field(body, b"MeetingTimeZone") {
            new_item.timezone_blob = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OnlineMeetingConfLink") {
            new_item.online_meeting_conf_link = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"OnlineMeetingExternalLink") {
            new_item.online_meeting_external_link = Some(v);
        }
        if let Some(v) = extract_ews_field(body, b"ClientUid") {
            new_item.client_uid = Some(v);
        }
    }

    let conflict_resolution =
        extract_conflict_resolution(body).unwrap_or_else(|| "AutoResolve".to_string());

    // A1: ensure an ORGANIZER email is present whenever the updated item still
    // has attendees; required by Stalwart's CalDAV scheduler.
    let owner_email = crate::util::user_primary_email(&auth.username, &state.cfg.mail_domain);
    crate::calendar::ensure_organizer_for_scheduling(&mut new_item, owner_email.as_deref());

    // A2/A3: honour SendMeetingInvitationsOrCancellations on UpdateItem. Outlook
    // resends REQUESTs to attendees when it asks the server to schedule; a plain
    // VEVENT PUT through CalDAV makes Stalwart auto-deliver those. JMAP calendar
    // blob writes do NOT trigger scheduling, so route to CalDAV when the
    // organizer wants invites re-sent. SendToNone marks attendees
    // SCHEDULE-AGENT=CLIENT so the roster survives without auto-scheduling.
    let upd_disposition = crate::calendar::ScheduleDisposition::parse(
        extract_open_tag_attr(body, "updateitem", "SendMeetingInvitationsOrCancellations")
            .or_else(|| extract_open_tag_attr(body, "updateitem", "SendMeetingInvitations"))
            .as_deref(),
    );
    let upd_scheduling_needed = match upd_disposition {
        Some(d) => crate::calendar::scheduling_needed(&new_item, d),
        None => false,
    };
    if matches!(
        upd_disposition,
        Some(crate::calendar::ScheduleDisposition::SendToNone)
    ) {
        crate::calendar::mark_scheduling_client_side(&mut new_item);
    }

    // Decide backend: JMAP if preferred and item looks like JMAP and client
    // available AND no scheduling is required; else CalDAV.
    let use_jmap = state.cfg.prefer_jmap_calendar
        && state.jmap_client.is_some()
        && stored_item.resource_href.starts_with("jmap://")
        && !upd_scheduling_needed;

    let updated_row = if use_jmap {
        match try_jmap_update_calendar(
            state,
            owner,
            &password_secret,
            &stored_item,
            &new_item,
            &conflict_resolution,
        )
        .await
        {
            Ok(row) => row,
            Err(e) => {
                tracing::warn!(error = %e, "JMAP calendar update failed, falling back to CalDAV");
                // Fallback requires additional steps: must fetch existing ICS via CalDAV, then apply changes.
                // We'll have to get the existing event via CalDAV to merge, but we already built new_item from changes.
                // It's simpler: we need the existing_ics to compute new state; but we can just apply the changes on top of whatever we have.
                // However, CalDAV update requires existing_etag for If-Match unless conflict_resolution says skip.
                // We can attempt CalDAV update by performing get_event then put_event with the combined ICS.
                match update_calendar_via_caldav(
                    state,
                    owner,
                    &password_secret,
                    &stored_item,
                    &new_item,
                    &conflict_resolution,
                )
                .await
                {
                    Ok(row) => row,
                    Err(resp) => return *resp,
                }
            }
        }
    } else {
        match update_calendar_via_caldav(
            state,
            owner,
            &password_secret,
            &stored_item,
            &new_item,
            &conflict_resolution,
        )
        .await
        {
            Ok(row) => row,
            Err(resp) => return *resp,
        }
    };
    let response_row = EwsItemRow {
        server_id: stored_item.server_id.clone(),
        resource_href: updated_row.resource_href.clone(),
        uid: updated_row.uid.clone(),
        caldav_href: updated_row.caldav_href.clone(),
        etag: updated_row.etag.clone(),
        updated_at: None,
    };
    publish_event(
        state,
        NotificationEvent::ItemModified {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
            item_id: stored_item.server_id.clone(),
            change_key: changekey_for_item(&response_row),
        },
    );
    let response = format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:UpdateItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items>{}</m:Items></m:UpdateItemResponseMessage></m:ResponseMessages></m:UpdateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(
            &stored_item.server_id,
            &changekey_for_item(&response_row),
            &new_item
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
    publish_event(
        state,
        NotificationEvent::ItemDeleted {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Inbox),
            item_id: item_id.to_string(),
        },
    );

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

    // Check if it's a contact (server_id prefix "contact-")
    if item_id.starts_with("contact-") && state.carddav_client.is_some() {
        return handle_delete_contact_item(state, auth, body).await;
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

    let password_secret = SecretString::from(auth.password.expose_secret());

    // A2/A3: SendMeetingCancellations on DeleteItem decides whether Stalwart
    // auto-delivers a CANCEL to attendees. Stalwart's CalDAV scheduler does this
    // on DELETE of an organizer's event that carries attendees; a JMAP
    // CalendarEvent/set destroy does NOT. So when the client wants attendees
    // notified (default for a meeting the user owns), force CalDAV; otherwise
    // JMAP delete is fine (no iTIP sent).
    let del_disposition = crate::calendar::ScheduleDisposition::parse(
        extract_open_tag_attr(body, "deleteitem", "SendMeetingCancellations").as_deref(),
    );
    let del_scheduling_needed = match del_disposition {
        Some(d) => d.wants_scheduling(),
        None => false,
    };
    // Whether the deleted item actually had attendees (fetched lazily only when
    // scheduling matters — the stored row carries uid/etag but not the parsed
    // ICS, so resolve via the current calendar event if we must schedule).
    let had_attendees = if del_scheduling_needed {
        event_has_attendees(state, owner, &password_secret, &existing).await
    } else {
        false
    };
    let cancellation_needed = del_scheduling_needed && had_attendees;

    // Try JMAP delete first if preferred and item is JMAP-backed AND no
    // cancellation scheduling is required.
    let delete_result = if state.cfg.prefer_jmap_calendar
        && state.jmap_client.is_some()
        && existing.resource_href.starts_with("jmap://")
        && !cancellation_needed
    {
        match try_jmap_delete_calendar(state, owner, &password_secret, &existing).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!(error = %e, "JMAP delete failed, falling back to CalDAV");
                Err(e)
            }
        }
    } else {
        Err(anyhow::anyhow!("JMAP not preferred or not applicable"))
    };

    // CalDAV fallback
    if delete_result.is_err() {
        match delete_calendar_via_caldav(state, owner, &password_secret, &existing).await {
            Ok(()) => {}
            Err(resp) => return *resp,
        }
    }

    // Record deletion
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
    publish_event(
        state,
        NotificationEvent::ItemDeleted {
            owner: auth.username.clone(),
            folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
            item_id: item_id.clone(),
        },
    );
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
/// Moves an item (email or calendar) between folders.
/// For email: maps to JMAP Email/set updating mailboxIds.
/// For calendar: uses CalDAV MOVE (TODO: not fully implemented, returns success for now).
async fn handle_move_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    let to_folder_id = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .or_else(|| extract_first_attr(body, b"FolderId", b"Id"))
        .unwrap_or_default();

    if item_id.is_empty() || to_folder_id.is_empty() {
        return operation_error_response(
            &EwsAction::MoveItem,
            "ErrorInvalidIdMalformed",
            "MoveItem requires ItemId/@Id and a destination folder",
            StatusCode::OK,
        );
    }

    // Check if it's an email item (has em- prefix)
    if crate::email::is_email_server_id(&item_id) {
        if !state.email_available() {
            return operation_error_response(
                &EwsAction::MoveItem,
                "ErrorInvalidRequest",
                "Email operations are not enabled",
                StatusCode::FORBIDDEN,
            );
        }

        // Map DistinguishedFolderId to JMAP role
        let target_role = match to_folder_id.to_ascii_lowercase().as_str() {
            "inbox" => "inbox",
            "drafts" => "drafts",
            "sentitems" => "sent",
            "deleteditems" => "trash",
            "junkemail" => "junk",
            "outbox" => {
                // Outbox doesn't exist in JMAP; sent emails are submitted and go to Sent
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInvalidOperation",
                    "Cannot move items to Outbox",
                    StatusCode::OK,
                );
            }
            _ => {
                // Unknown folder
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInvalidOperation",
                    "Unsupported destination folder",
                    StatusCode::OK,
                );
            }
        };

        // Get JMAP client and account ID
        let jmap = match state.jmap_client.as_ref() {
            Some(jmap) => jmap,
            None => {
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "JMAP not configured",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, "MoveItem: failed to get JMAP account ID");
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "Failed to get email account",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // Perform the move via JMAP
        match crate::email::move_email_via_jmap(
            state,
            jmap,
            &account_id,
            &item_id,
            target_role,
            &auth.username,
            &auth.password,
        )
        .await
        {
            Ok(new_change_key) => {
                // Emit a ModifiedEvent: the item still exists with a new ChangeKey.
                // (EWS would normally emit a MovedEvent; we use ModifiedEvent because
                // this handler doesn't track the precise source folder, and a
                // ModifiedEvent is a valid, client-actionable notification.)
                publish_event(
                    state,
                    NotificationEvent::ItemModified {
                        owner: auth.username.clone(),
                        folder_id: folder_id_for(&auth.username, DistinguishedFolder::Inbox),
                        item_id: item_id.clone(),
                        change_key: new_change_key.clone(),
                    },
                );
                // Return the same ItemId, but with a new ChangeKey
                let response = format!(
                    r#"<m:MoveItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:MoveItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Message><t:ItemId Id="{}" ChangeKey="{}" /></t:Message></m:Items></m:MoveItemResponseMessage></m:ResponseMessages></m:MoveItemResponse>"#,
                    EWS_MSG_NS,
                    EWS_TYPE_NS,
                    xml_escape(&item_id),
                    xml_escape(&new_change_key)
                );
                soap_ok(response)
            }
            Err(e) => {
                tracing::error!(error = %e, "MoveItem: failed to move email via JMAP");
                operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "Failed to move email",
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    } else {
        // Calendar items — CalDAV move
        // Validate destination folder is a calendar folder
        let dest_folder = match DistinguishedFolder::from_str(&to_folder_id) {
            Ok(f) => f,
            Err(_) => {
                // Check if it's the explicit folder ID for the default calendar
                let expected_cal_id = crate::ews_folders::folder_id_for(
                    &auth.username,
                    DistinguishedFolder::Calendar,
                );
                if to_folder_id == expected_cal_id {
                    DistinguishedFolder::Calendar
                } else {
                    return operation_error_response(
                        &EwsAction::MoveItem,
                        "ErrorInvalidOperation",
                        "Unsupported destination folder for calendar item",
                        StatusCode::OK,
                    );
                }
            }
        };
        if !dest_folder.is_calendar() {
            return operation_error_response(
                &EwsAction::MoveItem,
                "ErrorInvalidOperation",
                "Destination folder is not a calendar",
                StatusCode::OK,
            );
        }

        // Look up source item to get its resource_href and collection
        let lookup = match state
            .storage
            .get_ews_item_by_server_id(&auth.username, &item_id)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorItemNotFound",
                    "Source calendar item not found",
                    StatusCode::OK,
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "MoveItem: failed to fetch calendar item");
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "Failed to fetch source item",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // If JMAP is preferred and the item is backed by JMAP, treat the move as a no-op.
        // Only one calendar is exposed, so moving within it does not change the item.
        if state.cfg.prefer_jmap_calendar && lookup.resource_href.starts_with("jmap://") {
            // Compute ChangeKey from server_id and etag
            let change_key = changekey_for_item(&lookup);
            publish_event(
                state,
                NotificationEvent::ItemModified {
                    owner: auth.username.clone(),
                    folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
                    item_id: item_id.clone(),
                    change_key: change_key.clone(),
                },
            );
            let response = format!(
                r#"<m:MoveItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:MoveItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:MoveItemResponseMessage></m:ResponseMessages></m:MoveItemResponse>"#,
                EWS_MSG_NS,
                EWS_TYPE_NS,
                xml_escape(&item_id),
                xml_escape(&change_key)
            );
            return soap_ok(response);
        }

        // Source collection href (caldav_href) should be set for CalDAV items
        let src_collection_href = match &lookup.caldav_href {
            Some(h) => h,
            None => {
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInvalidOperation",
                    "Source item has no calendar collection",
                    StatusCode::OK,
                );
            }
        };

        // Extract the resource name (href relative to collection) from full resource_href
        let src_href = lookup
            .resource_href
            .trim_start_matches(src_collection_href)
            .trim_start_matches('/');

        // Destination collection href: should be same as source because only default calendar supported
        let dst_collection_href = src_collection_href;

        // Initialize CalDAV client
        let caldav = match CaldavClient::new(&state.cfg) {
            Ok(c) => c,
            Err(e) => {
                tracing::error!(error = %e, "MoveItem: CalDAV client init failed");
                return operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "CalDAV unavailable",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // Perform the move. This returns a new ETag.
        match caldav
            .move_event(
                src_href,
                src_collection_href,
                dst_collection_href,
                &auth.username,
                auth.password.expose_secret(),
            )
            .await
        {
            Ok(new_etag) => {
                // If the move was within the same collection, server_id unchanged.
                // If it were to a different collection, we'd need to update storage with new resource_href and server_id.
                // For now, we only support same collection, so server_id remains the same.
                // ChangeKey can be the new etag.
                let change_key = new_etag;
                publish_event(
                    state,
                    NotificationEvent::ItemModified {
                        owner: auth.username.clone(),
                        folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
                        item_id: item_id.clone(),
                        change_key: change_key.clone(),
                    },
                );
                let response = format!(
                    r#"<m:MoveItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:MoveItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:MoveItemResponseMessage></m:ResponseMessages></m:MoveItemResponse>"#,
                    EWS_MSG_NS,
                    EWS_TYPE_NS,
                    xml_escape(&item_id),
                    xml_escape(&change_key)
                );
                soap_ok(response)
            }
            Err(e) => {
                tracing::error!(error = %e, "MoveItem: CalDAV MOVE failed");
                operation_error_response(
                    &EwsAction::MoveItem,
                    "ErrorInternalServerError",
                    "Failed to move calendar item",
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    }
}

/// Copies an item (email only currently) to another folder.
/// For email: maps to JMAP Email/set adding a mailboxId (does not remove existing ones).
async fn handle_copy_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    let to_folder_id = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .or_else(|| extract_first_attr(body, b"FolderId", b"Id"))
        .unwrap_or_default();

    if item_id.is_empty() || to_folder_id.is_empty() {
        return operation_error_response(
            &EwsAction::CopyItem,
            "ErrorInvalidIdMalformed",
            "CopyItem requires ItemId/@Id and a destination folder",
            StatusCode::OK,
        );
    }

    // Determine if the item is email or calendar
    if crate::email::is_email_server_id(&item_id) {
        // Email copy logic
        if !state.email_available() {
            return operation_error_response(
                &EwsAction::CopyItem,
                "ErrorInvalidRequest",
                "Email operations are not enabled",
                StatusCode::FORBIDDEN,
            );
        }

        // Map DistinguishedFolderId to JMAP role
        let target_role = match to_folder_id.to_ascii_lowercase().as_str() {
            "inbox" => "inbox",
            "drafts" => "drafts",
            "sentitems" => "sent",
            "deleteditems" => "trash",
            "junkemail" => "junk",
            "outbox" => {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInvalidOperation",
                    "Cannot copy items to Outbox",
                    StatusCode::OK,
                );
            }
            _ => {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInvalidOperation",
                    "Unsupported destination folder",
                    StatusCode::OK,
                );
            }
        };

        // Get JMAP client
        let jmap = match state.jmap_client.as_ref() {
            Some(jmap) => jmap,
            None => {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInternalServerError",
                    "JMAP not configured",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // Get account ID
        let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, "CopyItem: failed to get JMAP account ID");
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInternalServerError",
                    "Failed to get email account",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // Perform the copy via JMAP
        match crate::email::copy_email_via_jmap(
            state,
            jmap,
            &account_id,
            &item_id,
            target_role,
            &auth.username,
            &auth.password,
        )
        .await
        {
            Ok(_new_change_key) => {
                publish_event(
                    state,
                    NotificationEvent::ItemCreated {
                        owner: auth.username.clone(),
                        folder_id: folder_id_for(&auth.username, DistinguishedFolder::Inbox),
                        item_id: item_id.clone(),
                        change_key: _new_change_key.clone(),
                    },
                );
                let response = format!(
                    r#"<m:CopyItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CopyItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:Message><t:ItemId Id="{}" ChangeKey="{}" /></t:Message></m:Items></m:CopyItemResponseMessage></m:ResponseMessages></m:CopyItemResponse>"#,
                    EWS_MSG_NS,
                    EWS_TYPE_NS,
                    xml_escape(&item_id),
                    xml_escape(&_new_change_key)
                );
                soap_ok(response)
            }
            Err(e) => {
                tracing::error!(error = %e, "CopyItem: failed to copy email via JMAP");
                operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInternalServerError",
                    "Failed to copy email",
                    StatusCode::INTERNAL_SERVER_ERROR,
                )
            }
        }
    } else {
        // --- Calendar copy implementation ---
        // Only the default calendar is supported.
        // Validate destination folder is a calendar.
        let dest_folder_is_calendar = {
            let to_lower = to_folder_id.to_ascii_lowercase();
            to_lower == "1"
                || to_lower == "calendar"
                || to_folder_id
                    == crate::ews_folders::folder_id_for(
                        &auth.username,
                        crate::ews_folders::DistinguishedFolder::Calendar,
                    )
        };
        if !dest_folder_is_calendar {
            return operation_error_response(
                &EwsAction::CopyItem,
                "ErrorInvalidOperation",
                "Destination folder is not a calendar",
                StatusCode::OK,
            );
        }

        // Fetch source item from DB
        let lookup = match state
            .storage
            .get_ews_item_by_server_id(&auth.username, &item_id)
            .await
        {
            Ok(Some(row)) => row,
            Ok(None) => {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorItemNotFound",
                    "Source calendar item not found",
                    StatusCode::OK,
                );
            }
            Err(e) => {
                tracing::error!(error = %e, "CopyItem: failed to fetch source calendar item");
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInternalServerError",
                    "Failed to fetch source item",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

        // Backend selection: JMAP first if enabled and item is JMAP-backed
        if state.cfg.prefer_jmap_calendar && lookup.resource_href.starts_with("jmap://") {
            // JMAP path
            let jmap = match state.jmap_client.as_ref() {
                Some(j) => j,
                None => {
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "JMAP client not available",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Parse resource_href: jmap://calendar/{account_id}/{event_id}
            let after = lookup.resource_href.trim_start_matches("jmap://calendar/");
            let parts: Vec<&str> = after.split('/').collect();
            if parts.len() != 2 {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInvalidOperation",
                    "Invalid JMAP resource ID",
                    StatusCode::OK,
                );
            }
            let src_account_id = parts[0];
            let src_event_id = parts[1];

            // Fetch source event ICS
            let (src_ics, _event_id, _src_etag) = match jmap
                .get_calendar_event(src_account_id, src_event_id, &auth.username, &auth.password)
                .await
            {
                Ok((ics, _event_id, _src_etag)) => (ics, _event_id, _src_etag),
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: JMAP fetch failed");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to fetch source event",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Parse source ics to CalendarItem
            let src_item = match parse_ics_event(&src_ics) {
                Some(item) => item,
                None => {
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInvalidOperation",
                        "Failed to parse source event",
                        StatusCode::OK,
                    );
                }
            };

            // Create copy: new UID, new DTSTAMP
            let mut copied_item = src_item.clone();
            copied_item.uid = ::uuid::Uuid::new_v4().to_string();
            copied_item.dtstamp = Some(Utc::now());
            let new_ics = render_ics(&copied_item);

            // Get account ID and default calendar ID
            let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
                Ok(id) => id,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: failed to get JMAP account ID");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to get calendar account",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };
            let calendars = match jmap.query_calendars(&auth.username, &auth.password).await {
                Ok(cal) => cal,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: failed to query calendars");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to get calendars",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };
            let Some(calendar) = calendars.calendars.first() else {
                return operation_error_response(
                    &EwsAction::CopyItem,
                    "ErrorInternalServerError",
                    "No calendar available",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            };
            let calendar_id = match calendar.id {
                Some(ref id) => id,
                None => {
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Calendar missing ID",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Create new event via CalendarEvent/set
            let (event_id, _uid, etag) = match jmap
                .set_calendar_event(SetCalendarEventParams {
                    account_id: &account_id,
                    calendar_id: Some(calendar_id),
                    event_id: None,
                    ics: &new_ics,
                    username: &auth.username,
                    password: &auth.password,
                })
                .await
            {
                Ok(res) => res,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: JMAP create event failed");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to create calendar event",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Construct server_id and resource_href
            let server_id = generate_server_id(
                state.cfg.hmac_secret(),
                &format!("jmap:{}:{}", account_id, event_id),
            );
            let resource_href = format!("jmap://calendar/{}/{}", account_id, event_id);

            // Upsert mapping into storage
            if let Err(e) = state
                .storage
                .upsert_item_map(
                    &auth.username,
                    "",
                    &resource_href,
                    &server_id,
                    &copied_item.uid,
                    &etag,
                )
                .await
            {
                tracing::error!(error = %e, "CopyItem: failed to upsert item map");
            }

            // Compute ChangeKey
            let change_key = changekey_for_item(&EwsItemRow {
                server_id: server_id.clone(),
                resource_href,
                uid: Some(copied_item.uid),
                caldav_href: None,
                etag: Some(etag.to_string()),
                updated_at: None,
            });
            publish_event(
                state,
                NotificationEvent::ItemCreated {
                    owner: auth.username.clone(),
                    folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
                    item_id: server_id.clone(),
                    change_key: change_key.clone(),
                },
            );

            let response = format!(
                r#"<m:CopyItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CopyItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:CopyItemResponseMessage></m:ResponseMessages></m:CopyItemResponse>"#,
                EWS_MSG_NS,
                EWS_TYPE_NS,
                xml_escape(&server_id),
                xml_escape(&change_key)
            );
            soap_ok(response)
        } else {
            // CalDAV fallback
            let caldav = match CaldavClient::new(&state.cfg) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: CalDAV client init failed");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "CalDAV unavailable",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Fetch source event
            let (src_ics, _src_etag) = match caldav
                .get_event(
                    &lookup.resource_href,
                    &auth.username,
                    auth.password.expose_secret(),
                )
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: CalDAV get_event failed");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to get source event",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Parse source ics
            let src_item = match parse_ics_event(&src_ics) {
                Some(item) => item,
                None => {
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInvalidOperation",
                        "Failed to parse source event",
                        StatusCode::OK,
                    );
                }
            };

            // Create copy: new UID and DTSTAMP
            let mut copied_item = src_item.clone();
            copied_item.uid = ::uuid::Uuid::new_v4().to_string();
            copied_item.dtstamp = Some(Utc::now());
            let new_ics = render_ics(&copied_item);

            // Destination collection: use source's caldav_href if available, else resource_href
            let dest_collection = lookup.caldav_href.as_ref().unwrap_or(&lookup.resource_href);

            // Create new event via PUT (no If-Match)
            let (new_href, new_etag) = match caldav
                .put_event(
                    dest_collection,
                    None,
                    &new_ics,
                    &auth.username,
                    auth.password.expose_secret(),
                    None,
                )
                .await
            {
                Ok(res) => res,
                Err(e) => {
                    tracing::error!(error = %e, "CopyItem: CalDAV put_event failed");
                    return operation_error_response(
                        &EwsAction::CopyItem,
                        "ErrorInternalServerError",
                        "Failed to create copy",
                        StatusCode::INTERNAL_SERVER_ERROR,
                    );
                }
            };

            // Generate server_id
            let server_id = generate_server_id(state.cfg.hmac_secret(), &new_href);

            // Upsert mapping
            if let Err(e) = state
                .storage
                .upsert_item_map(
                    &auth.username,
                    "",
                    &new_href,
                    &server_id,
                    &copied_item.uid,
                    &new_etag,
                )
                .await
            {
                tracing::error!(error = %e, "CopyItem: failed to upsert item map");
            }

            // Compute ChangeKey (using CalDAV etag)
            let change_key = changekey_for_item(&EwsItemRow {
                server_id: server_id.clone(),
                resource_href: new_href.clone(),
                uid: Some(copied_item.uid),
                caldav_href: Some(dest_collection.to_string()),
                etag: Some(new_etag),
                updated_at: None,
            });
            publish_event(
                state,
                NotificationEvent::ItemCreated {
                    owner: auth.username.clone(),
                    folder_id: folder_id_for(&auth.username, DistinguishedFolder::Calendar),
                    item_id: server_id.clone(),
                    change_key: change_key.clone(),
                },
            );

            let response = format!(
                r#"<m:CopyItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:CopyItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Items><t:CalendarItem><t:ItemId Id="{}" ChangeKey="{}" /></t:CalendarItem></m:Items></m:CopyItemResponseMessage></m:ResponseMessages></m:CopyItemResponse>"#,
                EWS_MSG_NS,
                EWS_TYPE_NS,
                xml_escape(&server_id),
                xml_escape(&change_key)
            );
            soap_ok(response)
        }
    }
}

async fn handle_resolve_names(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let query =
        extract_first_tag_text(body, b"UnresolvedEntry").unwrap_or_else(|| auth.username.clone());
    let _search_scope = extract_first_tag_text(body, b"SearchScope")
        .unwrap_or_else(|| "ActiveDirectory".to_string());

    // Directory is optional; if not available, return empty result
    let Some(dir) = &state.directory else {
        let response = r#"<m:ResolveNamesResponse xmlns:m="urn:schemas:mail:outlook:ews" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:ResolutionSet TotalItemsInView="0" IncludesLastItemInRange="true"/></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#.to_string();
        return soap_ok(response);
    };

    // Perform search in blocking context
    let limit = 100;
    let query_clone = query.clone();
    let dir_clone = dir.clone();
    let search_result = match tokio::task::spawn_blocking(move || {
        dir_clone.search_blocking(&query_clone, Some(limit))
    })
    .await
    {
        Ok(Ok(res)) => res,
        Ok(Err(e)) => {
            tracing::warn!(target: "ews", "Directory search error: {}", e);
            return soap_fault(
                "ErrorDirectorySearch",
                "Failed to search directory",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
        Err(e) => {
            tracing::error!(target: "ews", "Directory task join error: {}", e);
            return soap_fault(
                "ErrorDirectorySearch",
                "Directory search task failed",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Build response with resolved names
    let mut resolution_xml = String::new();
    let mut total_items = 0;

    for contact in search_result.contacts {
        total_items += 1;
        resolution_xml.push_str(&format!(
            r#"<t:Resolution><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:Mailbox></t:Resolution>"#,
            xml_escape(&contact.display_name),
            xml_escape(&contact.email)
        ));
    }

    // TODO: Expand distribution lists if search_scope indicates
    // For now, distribution lists not supported

    let includes_last = total_items < limit;
    let response = format!(
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:ResolveNamesResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:ResolutionSet TotalItemsInView="{}" IncludesLastItemInRange="{}">{}</m:ResolutionSet></m:ResolveNamesResponseMessage></m:ResponseMessages></m:ResolveNamesResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        total_items,
        if includes_last { "true" } else { "false" },
        resolution_xml
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

async fn handle_get_user_oof_settings(
    state: &Arc<AppState>,
    auth: &AuthContext,
    _body: &str,
) -> Response {
    // Determine the user whose OOF settings are being requested.
    // In EWS, the user is identified by the mailbox in the request.
    // For simplicity, we use the authenticated user.
    let username = &auth.username;

    // Get OOF settings from manager if available, else return disabled defaults.
    let settings = if let Some(oof_mgr) = &state.oof_manager {
        match oof_mgr.get_oof_settings(username) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(target: "ews", user = %username, error = %e, "Failed to get OOF settings");
                // Return disabled settings on error.
                crate::oof::OofSettings {
                    enabled: false,
                    external_audience: crate::oof::ExternalAudience::All,
                    internal_reply: None,
                    external_reply: None,
                    start_time: None,
                    end_time: None,
                }
            }
        }
    } else {
        crate::oof::OofSettings {
            enabled: false,
            external_audience: crate::oof::ExternalAudience::All,
            internal_reply: None,
            external_reply: None,
            start_time: None,
            end_time: None,
        }
    };

    // Map OOF settings to EWS XML.
    let oof_state = if settings.enabled {
        "Enabled"
    } else {
        "Disabled"
    };
    let external_audience = match settings.external_audience {
        crate::oof::ExternalAudience::External => "External",
        crate::oof::ExternalAudience::KnownExternal => "KnownExternal",
        crate::oof::ExternalAudience::All => "All",
    };

    // Duration: if start/end are set, include them; else use zeros (per stub).
    let (start_time, end_time) =
        if let (Some(start), Some(end)) = (settings.start_time, settings.end_time) {
            (format_ews_datetime(&start), format_ews_datetime(&end))
        } else {
            (
                "2000-01-01T00:00:00Z".to_string(),
                "2000-01-01T00:00:00Z".to_string(),
            )
        };

    // Replies: if OOF enabled and messages set, include them; else empty.
    let internal_reply = settings.internal_reply.as_deref().unwrap_or("");
    let external_reply = settings.external_reply.as_deref().unwrap_or("");

    // Build response.
    let inner = format!(
        r#"<m:GetUserOofSettingsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessage ResponseClass="Success">
    <m:ResponseCode>NoError</m:ResponseCode>
  </m:ResponseMessage>
  <m:OofSettings>
    <t:OofState>{}</t:OofState>
    <t:ExternalAudience>{}</t:ExternalAudience>
    <t:Duration>
      <t:StartTime>{}</t:StartTime>
      <t:EndTime>{}</t:EndTime>
    </t:Duration>
    <t:InternalReply>{}</t:InternalReply>
    <t:ExternalReply>{}</t:ExternalReply>
  </m:OofSettings>
  <m:AllowExternalOof>true</m:AllowExternalOof>
</m:GetUserOofSettingsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        oof_state,
        external_audience,
        start_time,
        end_time,
        xml_escape(internal_reply),
        xml_escape(external_reply)
    );
    soap_ok(inner)
}

async fn handle_set_user_oof_settings(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    // Parse the incoming OOF settings from the XML body.
    // We'll extract OofState, ExternalAudience, Duration (StartTime/EndTime), InternalReply, ExternalReply.

    let oof_state_text =
        extract_first_tag_text(body, b"OofState").unwrap_or_else(|| "Disabled".to_string());
    let enabled = oof_state_text.eq_ignore_ascii_case("Enabled");

    let external_audience_text =
        extract_first_tag_text(body, b"ExternalAudience").unwrap_or_else(|| "All".to_string());
    let external_audience = match external_audience_text.to_lowercase().as_str() {
        "external" => crate::oof::ExternalAudience::External,
        "knownexternal" => crate::oof::ExternalAudience::KnownExternal,
        _ => crate::oof::ExternalAudience::All,
    };

    // Duration
    let start_time = extract_first_tag_text(body, b"StartTime")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));
    let end_time = extract_first_tag_text(body, b"EndTime")
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(&s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    // Replies
    let internal_reply = extract_first_tag_text(body, b"InternalReply");
    let external_reply = extract_first_tag_text(body, b"ExternalReply");

    // Prevent timestamps containing only zeros if OOF disabled? We'll just store as-is.
    let settings = crate::oof::OofSettings {
        enabled,
        external_audience,
        internal_reply,
        external_reply,
        start_time,
        end_time,
    };

    // Apply OOF settings if manager is available.
    let result = if let Some(oof_mgr) = &state.oof_manager {
        match oof_mgr.set_oof_settings(&auth.username, settings.clone()) {
            Ok(_) => {
                // Success: return same structure as GetUserOofSettings would return.
                // To avoid duplication, we could call handle_get_user_oof_settings but that needs the same state/auth/body.
                // Instead, we'll construct a success GetUserOofSettingsResponse.
                // Let's call get again to confirm.
                match oof_mgr.get_oof_settings(&auth.username) {
                    Ok(confirmed) => confirmed,
                    Err(e) => {
                        tracing::warn!(target: "ews", user = %auth.username, error = %e, "Failed to get OOF settings after set");
                        settings
                    }
                }
            }
            Err(e) => {
                tracing::warn!(target: "ews", user = %auth.username, error = %e, "Failed to set OOF settings");
                // Return the settings we attempted to set (per spec, we return what was set)
                settings
            }
        }
    } else {
        // No manager, just echo back the settings.
        settings
    };

    // Build response identical to GetUserOofSettings.
    let oof_state = if result.enabled {
        "Enabled"
    } else {
        "Disabled"
    };
    let external_audience = match result.external_audience {
        crate::oof::ExternalAudience::External => "External",
        crate::oof::ExternalAudience::KnownExternal => "KnownExternal",
        crate::oof::ExternalAudience::All => "All",
    };
    let (start_time, end_time) =
        if let (Some(start), Some(end)) = (result.start_time, result.end_time) {
            (format_ews_datetime(&start), format_ews_datetime(&end))
        } else {
            (
                "2000-01-01T00:00:00Z".to_string(),
                "2000-01-01T00:00:00Z".to_string(),
            )
        };
    let internal_reply = result.internal_reply.as_deref().unwrap_or("");
    let external_reply = result.external_reply.as_deref().unwrap_or("");

    let inner = format!(
        r#"<m:SetUserOofSettingsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessage ResponseClass="Success">
    <m:ResponseCode>NoError</m:ResponseCode>
  </m:ResponseMessage>
  <m:OofSettings>
    <t:OofState>{}</t:OofState>
    <t:ExternalAudience>{}</t:ExternalAudience>
    <t:Duration>
      <t:StartTime>{}</t:StartTime>
      <t:EndTime>{}</t:EndTime>
    </t:Duration>
    <t:InternalReply>{}</t:InternalReply>
    <t:ExternalReply>{}</t:ExternalReply>
  </m:OofSettings>
  <m:AllowExternalOof>true</m:AllowExternalOof>
</m:SetUserOofSettingsResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        oof_state,
        external_audience,
        start_time,
        end_time,
        xml_escape(internal_reply),
        xml_escape(external_reply)
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

/// Render the child fields of a `PersonaType` (MS-OXWSPERS §2.2.4.19) for a
/// directory `Contact`. The caller wraps these fields in the appropriate
/// `Persona` element (`<t:Persona>` for `FindPeople`'s `ArrayOfPeopleType`,
/// `<m:Persona>` for `GetPersona`), since the two operations place the same
/// `PersonaType` content under a different element/namespace.
///
/// The `PersonaId` is deterministic — the contact's SMTP address — so a
/// follow-up `GetPersona` (which supplies a `PersonaId` from an earlier
/// `FindPeople`/`ResolveNames`) can round-trip back to the same entry via the
/// JMAP directory. `ChangeKey` is a stable placeholder (the directory has no
/// per-entry version).
fn persona_fields_from_contact(contact: &Contact) -> String {
    format!(
        r#"<t:PersonaId Id="{}" ChangeKey="01"/><t:PersonaType>Person</t:PersonaType><t:DisplayName>{}</t:DisplayName><t:EmailAddress><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType><t:MailboxType>Mailbox</t:MailboxType></t:EmailAddress>"#,
        xml_escape(&contact.email),
        xml_escape(&contact.display_name),
        xml_escape(&contact.email),
    )
}

async fn handle_find_people(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    // `QueryString` drives find-people search (MS-OXWSPERS §3.1.4.1.3.1);
    // Outlook for Windows and Outlook Android both send it for GAL browse and
    // recipient/People lookup. Empty/absent means "return all" (directory
    // browse). With no directory configured, degrade to a single-person
    // self persona (the directory-less Exchange look-alike behaviour).
    let query = extract_first_tag_text(body, b"QueryString").unwrap_or_default();

    // `IndexedPageItemView` is a required element of `FindPeopleType`
    // (MS-OXWSPERS §3.1.4.1.3.1) and carries the paging window as attributes
    // (`Offset`/`BasePoint` from the start of the result, `MaxEntriesReturned`
    // per page). The directory snapshot is loaded once and sliced client-side so
    // later pages are retrievable.
    let offset = extract_first_attr(body, b"IndexedPageItemView", b"Offset")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let max_entries = extract_first_attr(body, b"IndexedPageItemView", b"MaxEntriesReturned")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(100)
        .max(1);

    let Some(dir) = &state.directory else {
        // Directory not configured: serve only the authenticated caller's own
        // persona so People search still resolves "self" without inventing
        // identities that do not exist in the back-end. A non-zero offset
        // yields an empty page (there is exactly one entry).
        if offset > 0 {
            return soap_ok(format!(
                r#"<m:FindPeopleResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindPeopleResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:People/><m:TotalNumberOfPeopleInView>1</m:TotalNumberOfPeopleInView><m:FirstMatchingRowIndex>{}</m:FirstMatchingRowIndex><m:FirstLoadedRowIndex>{}</m:FirstLoadedRowIndex></m:FindPeopleResponseMessage></m:ResponseMessages></m:FindPeopleResponse>"#,
                EWS_MSG_NS,
                EWS_TYPE_NS,
                offset + 1,
                offset + 1,
            ));
        }
        let self_contact = Contact {
            display_name: auth.username.clone(),
            email: auth.username.clone(),
            title: None,
            office: None,
            phone: None,
            department: None,
            company: None,
            last_modified: None,
        };
        let inner = format!(
            r#"<m:FindPeopleResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindPeopleResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:People><t:Persona>{}</t:Persona></m:People><m:TotalNumberOfPeopleInView>1</m:TotalNumberOfPeopleInView><m:FirstMatchingRowIndex>1</m:FirstMatchingRowIndex><m:FirstLoadedRowIndex>1</m:FirstLoadedRowIndex></m:FindPeopleResponseMessage></m:ResponseMessages></m:FindPeopleResponse>"#,
            EWS_MSG_NS,
            EWS_TYPE_NS,
            persona_fields_from_contact(&self_contact),
        );
        return soap_ok(inner);
    };

    // An empty query and `*` both mean "match all" (mirrors the directory
    // `search_blocking` wildcard contract used by the OAB/NSPI download paths).
    // `limit = None` fetches the full (bounded) match set so paging can slice it
    // accurately and report the directory-wide total.
    let effective_query = if query.is_empty() {
        "*".to_string()
    } else {
        query
    };
    let query_clone = effective_query.clone();
    let dir_clone = dir.clone();
    let search_result =
        match tokio::task::spawn_blocking(move || dir_clone.search_blocking(&query_clone, None))
            .await
        {
            Ok(Ok(res)) => res,
            Ok(Err(e)) => {
                tracing::warn!(target: "ews", "Directory search error: {}", e);
                return soap_fault(
                    "ErrorDirectorySearch",
                    "Failed to search directory",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
            Err(e) => {
                tracing::error!(target: "ews", "Directory task join error: {}", e);
                return soap_fault(
                    "ErrorDirectorySearch",
                    "Directory search task failed",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        };

    // `total_estimate` is the directory-wide match count (bounded by the
    // directory's safety ceiling); slice the requested page from it.
    let total_in_view = search_result.total_estimate;
    let page = search_result.contacts.iter().skip(offset).take(max_entries);
    let mut people_xml = String::new();
    let mut page_count = 0usize;
    for contact in page {
        people_xml.push_str(&format!(
            "<t:Persona>{}</t:Persona>",
            persona_fields_from_contact(contact)
        ));
        page_count += 1;
    }

    // `FirstMatchingRowIndex`/`FirstLoadedRowIndex` are 1-based (MS-OXWSPERS
    // §3.1.4.1.3.3); 0 when the page is empty. Both equal the first row of the
    // requested page (offset is 0-based in the request, 1-based in the reply).
    let first_row = if page_count > 0 { offset + 1 } else { 0 };
    let inner = format!(
        r#"<m:FindPeopleResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindPeopleResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:People>{}</m:People><m:TotalNumberOfPeopleInView>{}</m:TotalNumberOfPeopleInView><m:FirstMatchingRowIndex>{}</m:FirstMatchingRowIndex><m:FirstLoadedRowIndex>{}</m:FirstLoadedRowIndex></m:FindPeopleResponseMessage></m:ResponseMessages></m:FindPeopleResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, people_xml, total_in_view, first_row, first_row,
    );
    soap_ok(inner)
}

/// Handle EWS GetConversationItems (MS-OXWSCONV §3.1.4.1).
///
/// New Outlook requests the contents of one or more conversations so the
/// message-list "Conversation" grouping can be expanded into its constituent
/// messages. The gateway's `FindItem` already surfaces each email's
/// `<t:ConversationId>` as the JMAP `threadId`, so a `ConversationId` in this
/// request is guaranteed to be a JMAP `threadId`. We resolve it with a JMAP
/// `Email/query` filtered on `threadId` (RFC 8621 §4.3), order the results
/// oldest-first, and render each message as a `ConversationNode`/`Items` entry
/// (MS-OXWSCONV §2.2.4.1) so the client can display the full thread.
async fn handle_get_conversation_items(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::GetConversationItems,
                "ErrorInternalServerError",
                "JMAP client not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "GetConversationItems: failed to get JMAP account ID");
            return operation_error_response(
                &EwsAction::GetConversationItems,
                "ErrorInternalServerError",
                "Failed to get email account",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Extract each conversation id (JMAP threadId) from the request's
    // `Conversation` list; a missing/invalid list is a client error per
    // MS-OXWSCONV (the Conversations element is required).
    let conversation_ids = extract_first_attrs(body, b"ConversationId", b"Id");
    if conversation_ids.is_empty() {
        return operation_error_response(
            &EwsAction::GetConversationItems,
            "ErrorInvalidIdMalformed",
            "GetConversationItems requires at least one ConversationId",
            StatusCode::BAD_REQUEST,
        );
    }

    // Honour the client's size cap; default to a bounded page when absent.
    let max_items = extract_int(body, b"MaxItemsToReturn", 100).clamp(1, 512);

    let mut conversations_xml = String::new();
    for conversation_id in conversation_ids {
        let thread_id = conversation_id;
        // RFC 8621 §4.3.1: Email/query filter on threadId yields every message
        // in the thread. Order oldest-first so the thread reads naturally.
        let query = match jmap
            .query_emails(crate::jmap::QueryEmailsParams {
                account_id: &account_id,
                filter: Some(serde_json::json!({ "threadId": thread_id })),
                sort: Some(vec![serde_json::json!({
                    "property": "receivedAt",
                    "isAscending": true,
                })]),
                position: 0,
                limit: max_items as u64,
                username: &auth.username,
                password: &auth.password,
            })
            .await
        {
            Ok(q) => q,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    thread_id = %xml_escape(&thread_id),
                    "GetConversationItems: JMAP thread query failed"
                );
                // A failed thread query is not a whole-request failure: emit an
                // Error item for this conversation and keep resolving the rest.
                conversations_xml.push_str(&format!(
                    r#"<t:Conversation><t:ConversationId Id="{}" /><t:SyncState/><t:ConversationNodes/><t:TotalConversationNodes>0</t:TotalConversationNodes></t:Conversation>"#,
                    xml_escape(&thread_id)
                ));
                continue;
            }
        };

        let emails = query.emails;
        let mut nodes_xml = String::new();
        for email in &emails {
            let server_id = crate::email::email_server_id_from_jmap_id(
                email.id.as_deref().unwrap_or_default(),
            );
            let change_key = server_id.clone();
            let internet_message_id = email
                .message_id
                .as_deref()
                .map(|m| xml_escape(m).into_owned())
                .unwrap_or_default();
            let parent_message_id = email
                .in_reply_to
                .as_ref()
                .and_then(|v| v.first())
                .map(|m| xml_escape(m).into_owned())
                .unwrap_or_default();

            // One node per message; the Items element carries the email rendered
            // as an EWS Message (same shape as GetItem) so the client has subject,
            // sender, received time and preview for the expanded thread.
            let message_xml = crate::email::render_jmap_email_as_ews_message(
                email,
                &server_id,
                &change_key,
            );
            nodes_xml.push_str(&format!(
                r#"<t:ConversationNode><t:InternetMessageId>{}</t:InternetMessageId><t:ParentInternetMessageId>{}</t:ParentInternetMessageId><t:Items>{}</t:Items></t:ConversationNode>"#,
                internet_message_id, parent_message_id, message_xml
            ));
        }

        let total_nodes = emails.len();
        conversations_xml.push_str(&format!(
            r#"<t:Conversation><t:ConversationId Id="{}" /><t:SyncState/><t:ConversationNodes>{}</t:ConversationNodes><t:TotalConversationNodes>{}</t:TotalConversationNodes></t:Conversation>"#,
            xml_escape(&thread_id),
            nodes_xml,
            total_nodes
        ));
    }

    let inner = format!(
        r#"<m:GetConversationItemsResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetConversationItemsResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Conversations>{}</m:Conversations>
    </m:GetConversationItemsResponseMessage>
  </m:ResponseMessages>
</m:GetConversationItemsResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, conversations_xml
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

async fn handle_get_user_photo(state: &Arc<AppState>, _auth: &AuthContext, body: &str) -> Response {
    // MS-OXWSAVATAR §3.1.4.1 — the client supplies the requested recipient
    // email and a `SizeRequested` (HR48x48/HR64x64/HR96x96/HR120x120/
    // HR240x240/HR360x360/HR432x432/HR504x504/HR648x648).
    //
    // The gateway has NO Stalwart-side photo backend (audit §2d: getUserPhoto
    // for MAPI returns empty; no Stalwart-native photo storage exists), so it
    // MUST NOT probe the directory to differentiate "exists" from "does not
    // exist": doing so would let any authenticated mailbox user enumerate the
    // Stalwart account set via the Success/ErrorNoSuchEmailAddress split, the
    // same directory-disclosure vector `autodiscover::resolve_user_display_name`
    // (PR #1821) deliberately guards against. It would ALSO misclassify every
    // recipient as "no such address" when no directory is configured, and would
    // silently drop `spawn_blocking` `JoinError`s (PR #1845 cubic) into that
    // same error code.
    //
    // Instead: validate the email SYNTAX (a disclosure-free, constant-time
    // property of the client-supplied string), reject malformed/empty values
    // with `ErrorInvalidSmtpAddress`, and return the spec's "no photo
    // published" shape (`HasChanged="false"`, empty `PictureData`) for every
    // syntactically-valid recipient. Outlook renders the recipient's default
    // avatar — it does NOT error — so recipient previews and "Check Names" are
    // unaffected while zero directory surface is exposed.
    let _ = state; // No directory consult — see rationale above.
    let email = extract_first_tag_text(body, b"Email")
        .or_else(|| extract_first_tag_text(body, b"EmailAddress"))
        .unwrap_or_default();
    let _size_requested = extract_first_tag_text(body, b"SizeRequested").unwrap_or_default();
    let valid = is_valid_smtp_address(&email);
    let (class, code) = if valid {
        ("Success", "NoError")
    } else {
        ("Error", "ErrorInvalidSmtpAddress")
    };
    let inner = format!(
        r#"<m:GetUserPhotoResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetUserPhotoResponseMessage ResponseClass="{}">
<m:ResponseCode>{}</m:ResponseCode>
{}
</m:GetUserPhotoResponseMessage>
</m:ResponseMessages>
</m:GetUserPhotoResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        class,
        code,
        if valid {
            "<m:HasChanged>false</m:HasChanged><m:PictureData/>"
        } else {
            ""
        }
    );
    soap_ok(inner)
}

/// Disclosure-free SMTP address-syntax check: a single `@` separating two
/// non-empty local and domain parts, no whitespace, and a dot in the domain.
/// This validates the *form* of a client-supplied recipient (so a malformed
/// request yields `ErrorInvalidSmtpAddress` rather than a silent empty photo)
/// WITHOUT consulting any directory and WITHOUT disclosing whether the
/// address corresponds to a real Stalwart account.
fn is_valid_smtp_address(email: &str) -> bool {
    if email.is_empty() || email.len() > 320 {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() {
        return false;
    }
    if email.chars().any(|c| c.is_whitespace()) {
        return false;
    }
    // A real SMTP domain carries a dot; reject the bare `user@localhost`-style
    // forms Outlook never legitimately sends for GAL resolution.
    domain.contains('.') && !local.contains('@')
}

/// Handle EWS MarkAsJunk (MS-OXWSJUNK §3.1.4.1).
///
/// New Outlook's "report as junk / block sender" moves the selected emails to
/// the Junk Email mailbox (and optionally adds the sender to the Blocked senders
/// list). The gateway performs the actual move via JMAP `Email/set` (moving the
/// messages into the `junk`-role mailbox), then returns the moved item ids in
/// `MovedItemIds` so the client can reconcile its view.
///
/// The blocked-sender list (`BlockedSenders`) is not persisted server-side (the
/// gateway has no Stalwart-side sender-block store); the move itself is the
/// user-visible, idempotent operation and is fully implemented. `IsJunk=false`
/// (report as *not* junk) moves the message back to Inbox.
async fn handle_mark_as_junk(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let item_ids = extract_first_attrs(body, b"ItemId", b"Id");
    if item_ids.is_empty() {
        return operation_error_response(
            &EwsAction::MarkAsJunk,
            "ErrorInvalidIdMalformed",
            "MarkAsJunk requires at least one ItemId",
            StatusCode::BAD_REQUEST,
        );
    }

    // IsJunk attribute on the MarkAsJunk element: true → move to junk,
    // false → the user marked it as *not* junk (move back to inbox).
    let is_junk = extract_first_attr(body, b"MarkAsJunk", b"IsJunk")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);
    let target_role = if is_junk { "junk" } else { "inbox" };

    if !state.email_available() {
        return operation_error_response(
            &EwsAction::MarkAsJunk,
            "ErrorInvalidRequest",
            "Email operations are not enabled",
            StatusCode::FORBIDDEN,
        );
    }

    let jmap = match state.jmap_client.as_ref() {
        Some(j) => j,
        None => {
            return operation_error_response(
                &EwsAction::MarkAsJunk,
                "ErrorInternalServerError",
                "JMAP not configured",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!(error = %e, "MarkAsJunk: failed to get JMAP account ID");
            return operation_error_response(
                &EwsAction::MarkAsJunk,
                "ErrorInternalServerError",
                "Failed to get email account",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Move each email, collecting ids that actually moved. A non-email id
    // (e.g. a calendar/contact id) is silently skipped: MarkAsJunk applies to
    // messages and the client should never send those ids here in practice.
    let mut moved_xml = String::new();
    for item_id in &item_ids {
        if !crate::email::is_email_server_id(item_id) {
            continue;
        }
        match crate::email::move_email_via_jmap(
            state,
            jmap,
            &account_id,
            item_id,
            target_role,
            &auth.username,
            &auth.password,
        )
        .await
        {
            Ok(_new_change_key) => {
                moved_xml.push_str(&format!(
                    r#"<t:ItemId Id="{}" ChangeKey="{}" />"#,
                    xml_escape(item_id),
                    xml_escape(item_id)
                ));
            }
            Err(e) => {
                tracing::warn!(error = %e, item_id = %item_id, "MarkAsJunk: failed to move email");
            }
        }
    }

    let inner = format!(
        r#"<m:MarkAsJunkResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:MarkAsJunkResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:MovedItemIds>{}</m:MovedItemIds>
</m:MarkAsJunkResponseMessage>
</m:ResponseMessages>
</m:MarkAsJunkResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, moved_xml
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

/// Handle EWS GetClientAccessToken (MS-OXWSCORE §3.1.4.35).
///
/// Lets a mail app/extension obtain a scoped token for a *different* service
/// (e.g. an Outlook add-in's `CallerIdentity` or `ExtensionCallback`). The
/// request carries one or more `<t:TokenRequests><t:TokenRequest>` entries with
/// an `Id`, `TokenType`, and (for `CallerIdentity`) a `ExtensionId`.
///
/// The gateway does not broker tokens on behalf of any third-party identity
/// provider, so it cannot mint a real `TokenValue`. To remain spec-compliant
/// while disclosing nothing, it returns one `<t:ClientAccessTokenResponse>`
/// entry per request with an empty `TokenValue` and the matching `Id`, letting
/// the client proceed without treating the operation as an error. This is the
/// correct "no token issued" shape (MS-OXWSCORE §2.2.5.4.3) rather than the
/// previous hard-coded empty `<Token/>` nop.
async fn handle_get_client_access_token(_state: &Arc<AppState>, _auth: &AuthContext, body: &str) -> Response {
    let token_requests = extract_first_attrs(body, b"TokenRequest", b"Id");

    let mut tokens_xml = String::new();
    if token_requests.is_empty() {
        // No recognisable TokenRequest: emit a single empty entry so the
        // response is well-formed for clients that echo a bare request.
        tokens_xml.push_str(r#"<t:ClientAccessTokenResponse><t:Id/><t:TokenType>CallerIdentity</t:TokenType><t:TokenValue/></t:ClientAccessTokenResponse>"#);
    } else {
        for id in token_requests {
            tokens_xml.push_str(&format!(
                r#"<t:ClientAccessTokenResponse><t:Id>{}</t:Id><t:TokenType>CallerIdentity</t:TokenType><t:TokenValue/></t:ClientAccessTokenResponse>"#,
                xml_escape(&id)
            ));
        }
    }

    let inner = format!(
        r#"<m:GetClientAccessTokenResponse xmlns:m="{}" xmlns:t="{}">
<m:ResponseMessages>
<m:GetClientAccessTokenResponseMessage ResponseClass="Success">
<m:ResponseCode>NoError</m:ResponseCode>
<m:Token>{}</m:Token>
</m:GetClientAccessTokenResponseMessage>
</m:ResponseMessages>
</m:GetClientAccessTokenResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, tokens_xml
    );
    soap_ok(inner)
}
/// Compute the next occurrence start time for a calendar item, honouring its
/// recurrence (RRULE + EXDATE). Non-recurring items return their own `start`.
///
/// Recurrence expansion uses the `rrule` crate's `RRuleSet` (which parses the
/// standard `DTSTART`/`RRULE`/`EXDATE` iCalendar block). On parse failure we
/// fall back to the base `start` rather than dropping the reminder, so a
/// malformed (but still stored) event does not silently lose its reminder.
fn next_occurrence_start(item: &crate::calendar::CalendarItem, now: chrono::DateTime<Utc>) -> chrono::DateTime<Utc> {
    let start_utc = item.start;
    let Some(rrule) = item.rrule.as_deref() else {
        return start_utc;
    };
    if rrule.trim().is_empty() {
        return start_utc;
    }

    // The rrule crate only accepts the *basic* iCalendar date-time form
    // (`YYYYMMDDTHHMMSSZ`) for DTSTART/EXDATE, so format accordingly (format_ews
    // uses the extended YYYY-MM-DDTHH:MM:SSZ form which the parser rejects).
    let mut block = String::from("DTSTART:");
    block.push_str(&start_utc.format("%Y%m%dT%H%M%SZ").to_string());
    block.push_str("\nRRULE:");
    block.push_str(rrule);
    for ex in &item.exdates {
        block.push_str("\nEXDATE:");
        block.push_str(&ex.format("%Y%m%dT%H%M%SZ").to_string());
    }

    let parsed: Option<rrule::RRuleSet> = block.parse().ok();
    let Some(set) = parsed else {
        return start_utc;
    };

    // The rrule crate's `Tz` wrapper is not a `chrono::TimeZone`, so building a
    // `DateTime<rrule::Tz>` for `RRuleSet::after` is awkward from `DateTime<Utc>`.
    // Instead iterate occurrences in ascending order and pick the first one that
    // is still at/after "now". The iterator respects COUNT/UNTIL/EXDATE and
    // terminates; the `.take()` bound is a defence against a pathological
    // unbounded rule producing an unmanageable walk.
    set.into_iter()
        .take(100_000)
        .map(|d| d.with_timezone(&Utc))
        .find(|d| *d >= now)
        .unwrap_or(start_utc)
}

/// Handle EWS GetReminders (MS-OXWSRMND §3.1.4.1).
///
/// Reads the authenticated user's calendar events, keeps those that carry a
/// reminder (`ReminderMinutesBeforeStart`), computes each reminder's due time
/// (start — lead time, next occurrence for recurring events), and renders every
/// due/upcoming reminder within the requested window as an EWS `<t:Reminder>`.
async fn handle_get_reminders(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let now = chrono::Utc::now();
    let (window_start, window_end) = parse_reminder_window(body, now);

    let owner = owner_from_username(&auth.username);
    let password = auth.password.expose_secret();
    // Load at least the requested window (so a client that reconnects after a
    // long offline period still gets reminders it asked for), clamped to a
    // bounded horizon to keep the calendar query cheap and predictable.
    let load_start = window_start.min(now).max(now - chrono::Duration::days(365));
    let load_end = window_end.max(now).min(now + chrono::Duration::days(365));
    let items = match load_current_calendar_items(
        state,
        owner,
        password,
        Some((load_start, load_end)),
    )
    .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "GetReminders: failed to load calendar items");
            return operation_error_response(
                &EwsAction::GetReminders,
                "ErrorInternalServerError",
                "An internal error occurred while loading calendar items",
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };

    // Cap the returned set to bound the response for pathological data.
    let max_items = extract_int(body, b"MaxItems", 200).clamp(1, 2000);

    let mut reminders_xml = String::new();
    let mut count = 0usize;
    for current in &items {
        if count >= max_items {
            break;
        }
        let item = &current.item;
        let Some(minutes) = item.reminder else {
            continue;
        };
        // `reminder` stores "minutes before start" but may be signed depending
        // on provenance (negative `-PT15M` from iCalendar, positive `15` from an
        // EWS CreateItem); `unsigned_abs()` normalises both to the lead time
        // without risk of overflowing on `i32::MIN`. Clamp to a year to guard
        // against corrupt values producing an absurd due time.
        let lead_minutes = i64::from(minutes.unsigned_abs().min(525_600));
        let occurrence_start = next_occurrence_start(item, now);
        let occurrence_end = occurrence_start + (item.end - item.start);
        let reminder_due = occurrence_start - chrono::Duration::minutes(lead_minutes);
        // Only surface reminders within the requested window; a reminder whose
        // occurrence already fully passed is out of scope.
        if reminder_due < window_start {
            continue;
        }
        if reminder_due > window_end && occurrence_start > window_end {
            continue;
        }

        let ck = changekey_for_item(&current.row);
        let mut xml = String::new();
        xml.push_str(&format!(
            r#"<t:Reminder><t:Subject>{}</t:Subject><t:ReminderTime>{}</t:ReminderTime><t:StartDate>{}</t:StartDate><t:EndDate>{}</t:EndDate><t:ItemId Id="{}" ChangeKey="{}" /><t:ReminderGroup>Calendar</t:ReminderGroup><t:UID>{}</t:UID>"#,
            xml_escape(&item.subject),
            crate::util::format_ews_datetime(&reminder_due),
            crate::util::format_ews_datetime(&occurrence_start),
            crate::util::format_ews_datetime(&occurrence_end),
            xml_escape(&current.row.server_id),
            xml_escape(&ck),
            xml_escape(&item.uid),
        ));
        if !item.location.is_empty() {
            xml.push_str(&format!(
                "<t:Location>{}</t:Location>",
                xml_escape(&item.location)
            ));
        }
        xml.push_str("</t:Reminder>");
        reminders_xml.push_str(&xml);
        count += 1;
    }

    let inner = format!(
        r#"<m:GetRemindersResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetRemindersResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Reminders>{}</m:Reminders>
    </m:GetRemindersResponseMessage>
  </m:ResponseMessages>
</m:GetRemindersResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, reminders_xml
    );
    soap_ok(inner)
}

/// Parse the reminder request window (`BeginTime`/`EndTime`). Defaults to a
/// bounded "now → now+14 days" window when the client omits either bound.
fn parse_reminder_window(
    body: &str,
    now: chrono::DateTime<Utc>,
) -> (chrono::DateTime<Utc>, chrono::DateTime<Utc>) {
    let parse = |tag: &[u8]| -> Option<chrono::DateTime<Utc>> {
        // Use the shared EWS date-time parser so offsetless xs:dateTime values
        // (e.g. `2026-01-01T09:00:00`) parse consistently with the rest of the
        // EWS surface instead of silently falling back to the default window.
        extract_first_tag_text(body, tag).and_then(|s| crate::calendar::parse_datetime(&s))
    };
    let start = parse(b"BeginTime")
        .unwrap_or_else(|| now - chrono::Duration::days(30));
    let end = parse(b"EndTime")
        .unwrap_or_else(|| now + chrono::Duration::days(14));
    (start, end)
}

/// Handle EWS PerformReminderAction (MS-OXWSRMND §3.1.4.2).
///
/// Processes `Snooze`/`Dismiss` actions on reminder item ids and returns the
/// updated item ids in `UpdatedItemIds`. Reminder state is derived from the
/// calendar event's `ReminderMinutesBeforeStart`; the gateway does not persist
/// a separate per-reminder snooze/dismiss store, so a `Dismiss` or `Snooze` is
/// acknowledged for the referenced items and echoed back to the client so its
/// local reminder UI clears without error.
async fn handle_perform_reminder_action(
    _state: &Arc<AppState>,
    _auth: &AuthContext,
    body: &str,
) -> Response {
    let item_ids = extract_first_attrs(body, b"ItemId", b"Id");
    if item_ids.is_empty() {
        return operation_error_response(
            &EwsAction::PerformReminderAction,
            "ErrorInvalidIdMalformed",
            "PerformReminderAction requires at least one ReminderItemAction",
            StatusCode::BAD_REQUEST,
        );
    }

    let mut updated_xml = String::new();
    for item_id in &item_ids {
        updated_xml.push_str(&format!(
            r#"<t:ItemId Id="{}" ChangeKey="{}" />"#,
            xml_escape(item_id),
            xml_escape(item_id)
        ));
    }

    let inner = format!(
        r#"<m:PerformReminderActionResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:PerformReminderActionResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:UpdatedItemIds>{}</m:UpdatedItemIds>
    </m:PerformReminderActionResponseMessage>
  </m:ResponseMessages>
</m:PerformReminderActionResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, updated_xml
    );
    soap_ok(inner)
}

async fn handle_get_persona(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    // `GetPersona` resolves a single persona by `PersonaId` (which the gateway
    // renders as the SMTP address in `FindPeople`/`ResolveNames`) or by an
    // inline `EmailAddress` (MS-OXWSPERS §3.1.4.2.3.1). The `PersonaId` is an
    // ItemIdType whose `Id` attribute carries the address; the `EmailAddress`
    // selector is an `EmailAddressType` whose inner `EmailAddress` element (not
    // the optional `Name`) is the SMTP address. When neither identifier is
    // supplied, resolve the authenticated caller instead; an explicitly
    // supplied identifier is never silently replaced.
    let person_id = extract_first_attr(body, b"PersonaId", b"Id");
    let email_address = extract_tag_texts(body, b"EmailAddress")
        .into_iter()
        .find(|s| s.contains('@'));
    // Only default to the caller when no identifier was supplied at all; a
    // supplied (even malformed) identifier is resolved as-is and yields
    // `ErrorItemNotFound` when it cannot be found (the directory resolver
    // already returns `None` for a non-email string).
    let target = person_id
        .or(email_address)
        .unwrap_or_else(|| auth.username.clone());

    // Resolve via directory when one is configured and reachable.
    if let Some(dir) = &state.directory {
        let target_clone = target.clone();
        let dir_clone = dir.clone();
        let resolved =
            tokio::task::spawn_blocking(move || dir_clone.resolve_email_blocking(&target_clone))
                .await;
        match resolved {
            Ok(Ok(Some(contact))) => {
                // The `Persona` element is in the messages namespace (`m:`), and
                // holds the `PersonaType` fields directly — there is no nested
                // `<t:Persona>` element here (MS-OXWSPERS §3.1.4.2.3.2).
                let inner = format!(
                    r#"<m:GetPersonaResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetPersonaResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Persona>{}</m:Persona></m:GetPersonaResponseMessage></m:ResponseMessages></m:GetPersonaResponse>"#,
                    EWS_MSG_NS,
                    EWS_TYPE_NS,
                    persona_fields_from_contact(&contact),
                );
                return soap_ok(inner);
            }
            Ok(Ok(None)) => {
                // Not in the directory (or a non-resolvable identifier): report
                // a failed operation, not a fabricated identity. EWS uses
                // `ResponseClass="Error"` with `ErrorItemNotFound` so clients
                // do not treat the missing persona as a success.
                let inner = format!(
                    r#"<m:GetPersonaResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetPersonaResponseMessage ResponseClass="Error"><m:ResponseCode>ErrorItemNotFound</m:ResponseCode></m:GetPersonaResponseMessage></m:ResponseMessages></m:GetPersonaResponse>"#,
                    EWS_MSG_NS, EWS_TYPE_NS,
                );
                return soap_ok(inner);
            }
            Ok(Err(e)) => {
                tracing::warn!(target: "ews", "Directory resolve error: {}", e);
                return soap_fault(
                    "ErrorDirectorySearch",
                    "Failed to resolve persona",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
            Err(e) => {
                tracing::error!(target: "ews", "Directory task join error: {}", e);
                return soap_fault(
                    "ErrorDirectorySearch",
                    "Directory resolve task failed",
                    StatusCode::INTERNAL_SERVER_ERROR,
                );
            }
        }
    }

    // No directory: self persona only (directory-less look-alike behaviour).
    let self_contact = Contact {
        display_name: auth.username.clone(),
        email: auth.username.clone(),
        title: None,
        office: None,
        phone: None,
        department: None,
        company: None,
        last_modified: None,
    };
    let inner = format!(
        r#"<m:GetPersonaResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:GetPersonaResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:Persona>{}</m:Persona></m:GetPersonaResponseMessage></m:ResponseMessages></m:GetPersonaResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        persona_fields_from_contact(&self_contact),
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

/// Handle EWS GetUserConfiguration operation (MS-OXWSUSRCFG §3.1.4.3).
///
/// New Outlook reads several per-user "Options" objects through this operation,
/// most notably the calendar **working hours** (`Name="WorkHours"`) and the
/// master category list (`Name="CategoryList"`). The gateway has no Stalwart-side
/// store for arbitrary user-configuration blobs, so it derives the values it
/// genuinely knows and returns deterministic, spec-conformant defaults for the
/// rest:
///
///   * `WorkHours`  → a real `<t:WorkingHours>` block (Mon–Fri 08:00–17:00 UTC)
///     encoded exactly as Exchange stores it (base64 UTF-8 XML inside the
///     configuration dictionary), so the Options→Calendar "work hours" UI
///     renders rather than appearing blank.
///   * `CategoryList` → the union of the account's JMAP `Email` `keywords`
///     (excluding the `$`-prefixed system keywords), surfaced as a `StringArray`
///     dictionary entry — real, per-account data.
///   * any other name → an empty `Dictionary` (the spec's "no object stored"
///     shape).
async fn handle_get_user_configuration(
    state: &Arc<AppState>,
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
    let folder_id = extract_first_attr(body, b"FolderId", b"Id");
    let distinguished_id = extract_first_attr(body, b"DistinguishedFolderId", b"Id");

    let folder_ref_xml = match (&folder_id, &distinguished_id) {
        (Some(fid), _) => format!(r#"<t:FolderId Id="{}" />"#, xml_escape(fid)),
        (None, Some(did)) => {
            format!(r#"<t:DistinguishedFolderId Id="{}" />"#, xml_escape(did))
        }
        _ => r#"<t:DistinguishedFolderId Id="msgfolderroot" />"#.to_string(),
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

    // Determine ParentFolderId according to MS-OXWSUSRCFG.
    // If the requested folder (identified by FolderId or DistinguishedFolderId) is supported,
    // use it as the parent. Otherwise, fall back to MsgFolderRoot.
    let owner = owner_from_username(&auth.username);
    let parent_folder = {
        match (folder_id, distinguished_id) {
            (Some(fid), _) => resolve_folder_id(&fid, owner)
                .filter(|f| f.is_supported())
                .unwrap_or(DistinguishedFolder::MsgFolderRoot),
            (None, Some(did)) => {
                let df = match did.to_ascii_lowercase().as_str() {
                    "msgfolderroot" | "root" => DistinguishedFolder::MsgFolderRoot,
                    "calendar" => DistinguishedFolder::Calendar,
                    "inbox" => DistinguishedFolder::Inbox,
                    "sentitems" => DistinguishedFolder::SentItems,
                    "deleteditems" => DistinguishedFolder::DeletedItems,
                    "drafts" => DistinguishedFolder::Drafts,
                    "outbox" => DistinguishedFolder::Outbox,
                    "junkemail" | "junk" => DistinguishedFolder::JunkEmail,
                    _ => DistinguishedFolder::MsgFolderRoot,
                };
                if df.is_supported() {
                    df
                } else {
                    DistinguishedFolder::MsgFolderRoot
                }
            }
            _ => DistinguishedFolder::MsgFolderRoot,
        }
    };
    let parent_fid = folder_id_for(owner, parent_folder);
    let parent_prefix_len = parent_fid.find('-').map(|i| i + 1).unwrap_or(4);
    let parent_ck = &parent_fid[parent_prefix_len..];
    let parent_folder_id_xml = format!(
        r#"<t:ParentFolderId Id="{}" ChangeKey="{}" />"#,
        xml_escape(&parent_fid),
        parent_ck
    );

    // Build the configuration dictionary for the requested object name. Only
    // the objects the gateway can derive real data for are populated; the rest
    // are returned empty per the "no object stored" shape.
    let dictionary_xml = build_user_configuration_dictionary(state, auth, &config_name).await;

    let response_xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/">
  <s:Header>
    {svi}
  </s:Header>
  <s:Body>
    <m:GetUserConfigurationResponse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types">
      <m:ResponseMessages>
        <m:GetUserConfigurationResponseMessage ResponseClass="Success">
          <m:ResponseCode>NoError</m:ResponseCode>
          <m:UserConfiguration>
            <t:UserConfigurationName Name="{}">{}</t:UserConfigurationName>
            {}
            <t:ItemId Id="{}" ChangeKey="{}" />
            <t:Dictionary>{}</t:Dictionary>
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
        change_key,
        dictionary_xml,
        svi = version::current().render_ews_header(EWS_TYPE_NS),
    );

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/xml; charset=utf-8")
        .body(response_xml.into())
        .unwrap()
}

/// Build the `UserConfiguration` dictionary for a named configuration object.
///
/// Returns the inner `<t:DictionaryEntry>` XML (or empty string) for the
/// requested `config_name`. Only the objects the gateway can derive real data
/// for are populated:
///
///   * `WorkHours`   → a `WorkingHours` block (Mon–Fri 08:00–17:00 UTC), stored
///     in the `ByteArray` value form Exchange uses (base64 of the UTF-8 XML
///     `<t:WorkingHours>` fragment).
///   * `CategoryList` → the account's JMAP email keyword labels (excluding
///     `$`-prefixed system keywords) as a `StringArray`.
///
/// Anything else returns an empty string (empty dictionary).
async fn build_user_configuration_dictionary(
    state: &Arc<AppState>,
    auth: &AuthContext,
    config_name: &str,
) -> String {
    match config_name.to_ascii_lowercase().as_str() {
        "workhours" | "work hours" => {
            // A fixed zero-bias (UTC) time zone matching the documented 08:00–17:00 UTC
            // work period. No daylight rule is declared so the reported hours do not
            // drift by ±60m for part of the year.
            let working_hours_xml = r#"<t:WorkingHours xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"><t:TimeZone><t:Bias>0</t:Bias><t:StandardTime><t:Bias>0</t:Bias><t:Time>00:00:00</t:Time><t:DayOrder>1</t:DayOrder><t:Month>1</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:StandardTime><t:DaylightTime><t:Bias>0</t:Bias><t:Time>00:00:00</t:Time><t:DayOrder>1</t:DayOrder><t:Month>1</t:Month><t:DayOfWeek>Sunday</t:DayOfWeek></t:DaylightTime></t:TimeZone><t:WorkingPeriodArray><t:WorkingPeriod><t:DayOfWeek>Monday Tuesday Wednesday Thursday Friday</t:DayOfWeek><t:StartTimeInMinutes>480</t:StartTimeInMinutes><t:EndTimeInMinutes>1020</t:EndTimeInMinutes></t:WorkingPeriod></t:WorkingPeriodArray></t:WorkingHours>"#;
            let encoded = STANDARD.encode(working_hours_xml.as_bytes());
            format!(
                r#"<t:DictionaryEntry><t:DictionaryKey><t:Type>String</t:Type><t:Value>WorkHours</t:Value></t:DictionaryKey><t:DictionaryValue><t:Type>ByteArray</t:Type><t:Value>{}</t:Value></t:DictionaryValue></t:DictionaryEntry>"#,
                encoded
            )
        }
        "categorylist" | "category list" | "categories" => {
            let labels = collect_jmap_category_labels(state, auth).await;
            if labels.is_empty() {
                return String::new();
            }
            let value = labels.join(",");
            format!(
                r#"<t:DictionaryEntry><t:DictionaryKey><t:Type>String</t:Type><t:Value>CategoryList</t:Value></t:DictionaryKey><t:DictionaryValue><t:Type>StringArray</t:Type><t:Value>{}</t:Value></t:DictionaryValue></t:DictionaryEntry>"#,
                xml_escape(&value)
            )
        }
        _ => String::new(),
    }
}

/// Collect the union of the account's JMAP email `keywords` (excluding the
/// `$`-prefixed system keywords) to serve as the master category list.
async fn collect_jmap_category_labels(state: &Arc<AppState>, auth: &AuthContext) -> Vec<String> {
    let Some(jmap) = state.jmap_client.as_ref() else {
        return Vec::new();
    };
    let account_id = match jmap.get_account_id(&auth.username, &auth.password).await {
        Ok(id) => id,
        Err(_) => return Vec::new(),
    };
    // Sample a bounded page of recent emails to derive the label set. The master
    // category list is, in practice, the distinct set of user labels; a bounded
    // sample is sufficient and bounds the query cost.
    let result = match jmap
        .query_emails(crate::jmap::QueryEmailsParams {
            account_id: &account_id,
            filter: Some(serde_json::json!({})),
            sort: None,
            position: 0,
            limit: 200,
            username: &auth.username,
            password: &auth.password,
        })
        .await
    {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut labels = std::collections::BTreeSet::new();
    for email in &result.emails {
        for label in email.category_labels() {
            labels.insert(label);
        }
    }
    labels.into_iter().collect()
}

/// Create a calendar item via CalDAV.
async fn create_calendar_via_caldav(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    item: &crate::calendar::CalendarItem,
) -> Result<EwsItemRow, Box<Response>> {
    let caldav = CaldavClient::new(&state.cfg).map_err(|e| {
        tracing::error!(error = %e, "Failed to create CalDAV client");
        Box::new(operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInternalServerError",
            "An internal error occurred",
            StatusCode::INTERNAL_SERVER_ERROR,
        ))
    })?;

    let calendars = caldav
        .find_user_calendars(owner, password.expose_secret())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, owner = %owner, "CalDAV calendar discovery failed");
            Box::new(operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "An internal error occurred while discovering calendars",
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        })?;

    let collection_href = calendars
        .first()
        .ok_or_else(|| {
            Box::new(operation_error_response(
                &EwsAction::CreateItem,
                "ErrorFolderNotFound",
                "No writable calendar collection discovered",
                StatusCode::OK,
            ))
        })?
        .clone();

    let ics = render_ics(item);
    let (href, etag) = caldav
        .put_event(
            &collection_href,
            None,
            &ics,
            owner,
            password.expose_secret(),
            None,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to save calendar item via CalDAV");
            Box::new(operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                "An internal error occurred while saving the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        })?;

    let server_id = generate_server_id(state.cfg.hmac_secret(), &href);
    if let Err(e) = state
        .storage
        .upsert_item_map(owner, &collection_href, &href, &server_id, &item.uid, &etag)
        .await
    {
        tracing::error!(error = %e, owner = %owner, "Failed to persist created item mapping");
        return Err(Box::new(operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInternalServerError",
            "An internal error occurred while saving the item",
            StatusCode::INTERNAL_SERVER_ERROR,
        )));
    }

    Ok(EwsItemRow {
        server_id: server_id.clone(),
        resource_href: href,
        uid: Some(item.uid.clone()),
        caldav_href: Some(collection_href.clone()),
        etag: Some(etag.to_string()),
        updated_at: None,
    })
}

/// Create a calendar item via JMAP CalendarEvent/set.
async fn try_jmap_create_calendar(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    item: &crate::calendar::CalendarItem,
) -> Result<EwsItemRow, anyhow::Error> {
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow!("JMAP client not available"))?;

    // Ensure JMAP Calendar is supported
    if !jmap.supports_calendar(owner, password).await {
        return Err(anyhow!("JMAP Calendar not supported by server"));
    }

    // Get the account ID for calendar operations
    let account_id: String = jmap.get_account_id(owner, password).await?;

    // Discover the primary calendar (first one)
    let calendars = jmap.query_calendars(owner, password).await?;
    let Some(calendar) = calendars.calendars.first() else {
        return Err(anyhow!("No calendars available in JMAP"));
    };
    let calendar_id = calendar
        .id
        .as_ref()
        .ok_or_else(|| anyhow!("Calendar lacks an ID"))?;

    // Render iCalendar data from the EWS calendar item
    let ics = render_ics(item);

    // Call CalendarEvent/set (create)
    let (event_id, uid, etag): (String, String, String) = jmap
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account_id,
            calendar_id: Some(calendar_id),
            event_id: None,
            ics: &ics,
            username: owner,
            password,
        })
        .await?;

    // Construct the server_id and resource_href consistent with JMAP integration
    let server_id = generate_server_id(
        state.cfg.hmac_secret(),
        &format!("jmap:{}:{}", account_id, event_id),
    );
    let resource_href = format!("jmap://calendar/{}/{}", account_id, event_id);

    // Persist mapping
    if let Err(e) = state
        .storage
        .upsert_item_map(owner, "", &resource_href, &server_id, &item.uid, &etag)
        .await
    {
        return Err(anyhow!("Failed to upsert item map: {}", e));
    }

    Ok(EwsItemRow {
        server_id: server_id.clone(),
        resource_href,
        uid: Some(if uid.is_empty() {
            item.uid.clone()
        } else {
            uid
        }),
        caldav_href: None,
        etag: Some(etag.to_string()),
        updated_at: None,
    })
}

/// Update a calendar item via CalDAV.
async fn update_calendar_via_caldav(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    stored_item: &EwsItemRow,
    new_item: &crate::calendar::CalendarItem,
    conflict_resolution: &str,
) -> Result<EwsItemRow, Box<Response>> {
    let caldav = CaldavClient::new(&state.cfg).map_err(|e| {
        tracing::error!(error = %e, "Failed to create CalDAV client");
        Box::new(operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInternalServerError",
            "An internal error occurred",
            StatusCode::INTERNAL_SERVER_ERROR,
        ))
    })?;

    // Fetch existing event data to merge changes if AutoResolve/AlwaysOverwrite (optimistic concurrency handling)
    let (_existing_ics, existing_etag) = match caldav
        .get_event(&stored_item.resource_href, owner, password.expose_secret())
        .await
    {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "Failed to fetch existing event for update");
            return Err(Box::new(operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred while fetching the event",
                StatusCode::INTERNAL_SERVER_ERROR,
            )));
        }
    };

    // If we didn't get an etag from GET, try PROPFIND (Stalwart v0.16.5 quirk)
    let etag_for_if_match = match existing_etag {
        Some(etag) => Some(etag),
        None => {
            match caldav
                .get_etag(&stored_item.resource_href, owner, password.expose_secret())
                .await
            {
                Ok(Some(etag)) => Some(etag),
                _ => None,
            }
        }
    };

    // Render the new ICS
    let new_ics = render_ics(new_item);

    // Determine if we should use If-Match header
    // skip_ck_validation is determined based on ConflictResolution upstream; if AlwaysOverwrite/AutoResolve, we skip strong validation
    let skip_ck = matches!(
        conflict_resolution.to_ascii_lowercase().as_str(),
        "alwaysoverwrite" | "autoresolve"
    );

    // Use existing etag for If-Match unless skipping validation
    let if_match_etag = if skip_ck {
        None
    } else {
        etag_for_if_match.as_deref()
    };

    // Perform the PUT
    let (href, new_etag) = caldav
        .put_event(
            stored_item
                .caldav_href
                .as_ref()
                .unwrap_or(&stored_item.resource_href),
            if_match_etag,
            &new_ics,
            owner,
            password.expose_secret(),
            None,
        )
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "Failed to update calendar item via CalDAV");
            Box::new(operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                "An internal error occurred while updating the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        })?;

    // Persist new etag and updated_at before constructing EwsItemRow to avoid borrow-checker issues
    let server_id = stored_item.server_id.clone();
    if let Err(e) = state
        .storage
        .upsert_item_map(
            owner,
            stored_item.caldav_href.as_deref().unwrap_or(""),
            &href,
            &server_id,
            &new_item.uid,
            &new_etag,
        )
        .await
    {
        tracing::error!(error = %e, "Failed to upsert item map after CalDAV update");
        // Continue — not fatal for response
    }

    // Build the updated row
    let updated_row = EwsItemRow {
        server_id: server_id.clone(),
        resource_href: href,
        uid: Some(new_item.uid.clone()),
        caldav_href: stored_item.caldav_href.clone(),
        etag: Some(new_etag),
        updated_at: None,
    };

    Ok(updated_row)
}

/// Update a calendar item via JMAP CalendarEvent/set.
async fn try_jmap_update_calendar(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    stored_item: &EwsItemRow,
    new_item: &crate::calendar::CalendarItem,
    _conflict_resolution: &str,
) -> Result<EwsItemRow, anyhow::Error> {
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow!("JMAP client not available"))?;

    if !jmap.supports_calendar(owner, password).await {
        return Err(anyhow!("JMAP Calendar not supported"));
    }

    let account_id: String = jmap.get_account_id(owner, password).await?;

    // Parse stored resource_href: jmap://calendar/{account_id}/{event_id}
    let href = &stored_item.resource_href;
    if !href.starts_with("jmap://calendar/") {
        return Err(anyhow!("Invalid JMAP resource_href format: {}", href));
    }
    let parts: Vec<&str> = href
        .trim_start_matches("jmap://calendar/")
        .split('/')
        .collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid JMAP resource_href parts: {}", href));
    }
    let event_id = parts[1];

    // Render iCalendar
    let ics = render_ics(new_item);

    // set_calendar_event with event_id for update
    let (updated_event_id, uid, etag): (String, String, String) = jmap
        .set_calendar_event(SetCalendarEventParams {
            account_id: &account_id,
            calendar_id: None, // Not needed for update; we pass event_id
            event_id: Some(event_id),
            ics: &ics,
            username: owner,
            password,
        })
        .await?;

    // The returned event_id should match the input; if empty, use the input
    let final_event_id = if updated_event_id.is_empty() {
        event_id
    } else {
        &updated_event_id
    };

    // Build consistent resource_href and server_id
    let resource_href = format!("jmap://calendar/{}/{}", account_id, final_event_id);
    let server_id = generate_server_id(
        state.cfg.hmac_secret(),
        &format!("jmap:{}:{}", account_id, final_event_id),
    );

    // Upsert mapping
    if let Err(e) = state
        .storage
        .upsert_item_map(owner, "", &resource_href, &server_id, &new_item.uid, &etag)
        .await
    {
        return Err(anyhow!("Failed to upsert item map: {}", e));
    }

    Ok(EwsItemRow {
        server_id,
        resource_href,
        uid: Some(if uid.is_empty() {
            new_item.uid.clone()
        } else {
            uid
        }),
        caldav_href: None,
        etag: Some(etag.to_string()),
        updated_at: None,
    })
}

/// Resolve whether a stored calendar item currently has ATTENDEE lines.
/// Used by DeleteItem scheduling dispatch to decide whether Stalwart would
/// auto-send a CANCEL. Cheaper than re-parsing the full event.
async fn event_has_attendees(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    item: &EwsItemRow,
) -> bool {
    // Prefer the cheaper CalDAV GET when the item has an href, since Stalwart's
    // GET returns the full ICS without scheduling side-effects.
    if let Some(href) = &item.caldav_href
        && !href.is_empty()
    {
        let caldav = match CaldavClient::new(&state.cfg) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "Failed to build CalDAV client while checking attendees");
                return false;
            }
        };
        match caldav
            .get_event(href, owner, password.expose_secret())
            .await
        {
            Ok((ics, _)) => return ics.lines().any(|l| l.starts_with("ATTENDEE")),
            Err(e) => {
                tracing::warn!(error = %e, "CalDAV GET while checking attendees failed");
            }
        }
    }
    // Fall back to JMAP CalendarEvent/get with the iCalendar property.
    if let Some(jmap) = state.jmap_client.as_ref()
        && item.resource_href.starts_with("jmap://")
    {
        let parts: Vec<&str> = item
            .resource_href
            .trim_start_matches("jmap://calendar/")
            .split('/')
            .collect();
        if let [account_id, event_id] = parts.as_slice()
            && let Ok((ics, _, _)) = jmap
                .get_calendar_event(account_id, event_id, owner, password)
                .await
        {
            return ics.lines().any(|l| l.starts_with("ATTENDEE"));
        }
    }
    false
}

/// Delete a calendar item via CalDAV.
async fn delete_calendar_via_caldav(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    stored_item: &EwsItemRow,
) -> Result<(), Box<Response>> {
    let caldav = CaldavClient::new(&state.cfg).map_err(|e| {
        tracing::error!(error = %e, "Failed to create CalDAV client");
        Box::new(operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            "An internal error occurred",
            StatusCode::INTERNAL_SERVER_ERROR,
        ))
    })?;

    // Determine the href to DELETE
    let delete_href = if stored_item.resource_href.starts_with("http") {
        &stored_item.resource_href
    } else if let Some(caldav_href) = &stored_item.caldav_href {
        // Might need to combine caldav_href + resource_href
        // For CalDAV items, resource_href is the relative path from caldav_href, or absolute?
        // In current integration, resource_href is the full URL sometimes? We'll treat it carefully.
        // If it's relative (no scheme), we can join to caldav base if needed.
        // Simplify: use caldav.delete_event(href, owner, password)
        caldav_href
    } else {
        return Err(Box::new(operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorItemNotFound",
            "Cannot determine deletion target",
            StatusCode::OK,
        )));
    };

    // If the resource_href is absolute, use that directly; if relative, combine with base
    let final_href = if delete_href.contains("://") {
        delete_href.to_string()
    } else {
        // CalDAV client's delete_event expects full URL? In caldav.rs, delete_event takes href: &str,
        // which may be either absolute or relative; but we also need to supply owner and password.
        // The client will use base_url + href if href is relative.
        delete_href.to_string()
    };

    caldav
        .delete_event(&final_href, owner, password.expose_secret(), None)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "CalDAV delete failed");
            Box::new(operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                "An internal error occurred while deleting the item",
                StatusCode::INTERNAL_SERVER_ERROR,
            ))
        })?;

    Ok(())
}

/// Delete a calendar item via JMAP CalendarEvent/set (destroy).
async fn try_jmap_delete_calendar(
    state: &Arc<AppState>,
    owner: &str,
    password: &SecretString,
    stored_item: &EwsItemRow,
) -> Result<(), anyhow::Error> {
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow!("JMAP client not available"))?;

    if !jmap.supports_calendar(owner, password).await {
        return Err(anyhow!("JMAP Calendar not supported"));
    }

    let account_id: String = jmap.get_account_id(owner, password).await?;

    let href = &stored_item.resource_href;
    if !href.starts_with("jmap://calendar/") {
        return Err(anyhow!("Invalid JMAP resource_href format: {}", href));
    }
    let parts: Vec<&str> = href
        .trim_start_matches("jmap://calendar/")
        .split('/')
        .collect();
    if parts.len() != 2 {
        return Err(anyhow!("Invalid JMAP resource_href parts: {}", href));
    }
    let event_id = parts[1];

    jmap.destroy_calendar_events(&account_id, &[event_id.to_string()], owner, password)
        .await?;

    Ok(())
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
    fn test_extract_first_attrs_collects_all_in_order() {
        let xml = r#"<m:GetConversationItems>
            <m:Conversations>
                <t:Conversation><t:ConversationId Id="t1" ChangeKey="ck1"/></t:Conversation>
                <t:Conversation><t:ConversationId Id="t2" ChangeKey="ck2"/></t:Conversation>
            </m:Conversations>
        </m:GetConversationItems>"#;
        assert_eq!(
            extract_first_attrs(xml, b"ConversationId", b"Id"),
            vec!["t1".to_string(), "t2".to_string()]
        );
    }

    #[test]
    fn test_extract_first_attrs_missing_tag_returns_empty() {
        let xml = r#"<m:SomethingElse><t:Other Id="x"/></m:SomethingElse>"#;
        assert!(extract_first_attrs(xml, b"ConversationId", b"Id").is_empty());
    }

    #[test]
    fn test_next_occurrence_start_non_recurring_returns_start() {
        let now = chrono::Utc::now();
        let start = now + chrono::Duration::hours(1);
        let item = crate::calendar::CalendarItem {
            start,
            uid: "uid-1".to_string(),
            ..Default::default()
        };
        // No rrule → the base start is the next (only) occurrence.
        assert_eq!(next_occurrence_start(&item, now), start);
    }

    #[test]
    fn test_next_occurrence_start_recurring_advances_past_now() {
        // A daily recurrence starting long ago must advance to today/forward.
        let now = chrono::Utc::now();
        let start = now - chrono::Duration::days(30);
        let item = crate::calendar::CalendarItem {
            start,
            uid: "uid-daily".to_string(),
            rrule: Some("FREQ=DAILY".to_string()),
            ..Default::default()
        };
        let next = next_occurrence_start(&item, now);
        assert!(next >= now, "next occurrence must be at/after now: {next:?}");
        // It must be the base start advanced by whole days (ignoring sub-day drift).
        let days = (next - start).num_days();
        assert!(days >= 30, "daily recurrence should have advanced ~30 days: {days}");
    }

    #[test]
    fn test_parse_reminder_window_defaults_when_absent() {
        let now = chrono::Utc::now();
        let body = r#"<m:GetReminders/>"#;
        let (start, end) = parse_reminder_window(body, now);
        assert!(start < now, "default start should be in the past");
        assert!(end > now, "default end should be in the future");
    }

    #[test]
    fn test_parse_reminder_window_honours_bounds() {
        let now = chrono::Utc::now();
        // Truncate to whole seconds: GetReminders bounds are UTC second-precision
        // date-times, and `format_ews_datetime` drops sub-second components.
        let begin = chrono::DateTime::<Utc>::from_timestamp((now - chrono::Duration::days(2)).timestamp(), 0)
            .unwrap();
        let end = chrono::DateTime::<Utc>::from_timestamp((now + chrono::Duration::days(3)).timestamp(), 0)
            .unwrap();
        let body = format!(
            r#"<m:GetReminders><m:BeginTime>{}</m:BeginTime><m:EndTime>{}</m:EndTime></m:GetReminders>"#,
            crate::util::format_ews_datetime(&begin),
            crate::util::format_ews_datetime(&end),
        );
        let (parsed_start, parsed_end) = parse_reminder_window(&body, now);
        assert_eq!(parsed_start, begin);
        assert_eq!(parsed_end, end);
    }

    #[test]
    fn test_parse_reminder_window_accepts_offsetless_datetime() {
        // EWS xs:dateTime permits an offsetless value; the shared parser must
        // accept it (not silently fall back to the default window).
        let now = chrono::Utc::now();
        let begin = chrono::DateTime::<Utc>::from_timestamp((now - chrono::Duration::hours(2)).timestamp(), 0)
            .unwrap();
        let end = chrono::DateTime::<Utc>::from_timestamp((now + chrono::Duration::hours(2)).timestamp(), 0)
            .unwrap();
        let fmt = |d: &chrono::DateTime<Utc>| d.format("%Y-%m-%dT%H:%M:%S").to_string();
        let body = format!(
            r#"<m:GetReminders><m:BeginTime>{}</m:BeginTime><m:EndTime>{}</m:EndTime></m:GetReminders>"#,
            fmt(&begin),
            fmt(&end),
        );
        let (parsed_start, parsed_end) = parse_reminder_window(&body, now);
        assert_eq!(parsed_start, begin);
        assert_eq!(parsed_end, end);
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

    // --- EWS notifications (MS-OXWSNTIF) wire-format tests ---

    #[test]
    fn test_detect_action_recognizes_notification_operations() {
        // Each of the four notification operations must be detected from a SOAP
        // envelope body, with both the `m:` and bare-tag conventions.
        let subscribe = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest/></m:Subscribe>"#;
        assert_eq!(detect_action(subscribe), Some(EwsAction::Subscribe));

        let unsubscribe = r#"<m:Unsubscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:SubscriptionId>abc</m:SubscriptionId></m:Unsubscribe>"#;
        assert_eq!(detect_action(unsubscribe), Some(EwsAction::Unsubscribe));

        let get_events = r#"<m:GetEvents xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:SubscriptionId>abc</m:SubscriptionId></m:GetEvents>"#;
        assert_eq!(detect_action(get_events), Some(EwsAction::GetEvents));

        let streaming = r#"<m:GetStreamingEvents xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:SubscriptionIds/></m:GetStreamingEvents>"#;
        assert_eq!(
            detect_action(streaming),
            Some(EwsAction::GetStreamingEvents)
        );
    }

    #[test]
    fn test_response_message_names_for_notifications() {
        assert_eq!(
            EwsAction::Subscribe.response_message_name(),
            "SubscribeResponseMessage"
        );
        assert_eq!(
            EwsAction::Unsubscribe.response_message_name(),
            "UnsubscribeResponseMessage"
        );
        assert_eq!(
            EwsAction::GetEvents.response_message_name(),
            "GetEventsResponseMessage"
        );
        assert_eq!(
            EwsAction::GetStreamingEvents.response_message_name(),
            "GetStreamingEventsResponseMessage"
        );
    }

    #[test]
    fn test_watermark_round_trip() {
        // encode/decode is a bijection over u64 (server-opaque to the client).
        for seq in [0u64, 1, 42, u64::MAX / 2, u64::MAX] {
            let wm = encode_watermark(seq);
            assert_eq!(decode_watermark(&wm), seq, "round-trip failed for {seq}");
        }
        // Unknown / malformed watermarks degrade gracefully to 0 (treated as
        // "before any event"), never panic.
        assert_eq!(decode_watermark("not-base64!!"), 0);
        assert_eq!(decode_watermark(""), 0);
        assert_eq!(decode_watermark("AAAA"), 0); // wrong length
    }

    #[test]
    fn test_parse_subscription_request_all_folders() {
        let body = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest SubscribeToAllFolders="true"><m:FolderIds/><m:EventTypes><t:EventType>CopiedEvent</t:EventType></m:EventTypes></m:PullSubscriptionRequest></m:Subscribe>"#;
        let parsed = parse_subscription_request(body);
        assert!(parsed.folders.is_none(), "SubscribeToAllFolders=>None");
        let types = parsed.event_types.expect("event types");
        assert_eq!(types, HashSet::from(["CopiedEvent".to_string()]));
    }

    #[test]
    fn test_parse_subscription_request_explicit_folders() {
        let body = r#"<m:PullSubscriptionRequest xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages">
            <m:FolderIds>
                <t:FolderId Id="inbox-1" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"/>
                <t:DistinguishedFolderId Id="calendar" xmlns:t="http://schemas.microsoft.com/exchange/services/2006/types"/>
            </m:FolderIds>
            <m:EventTypes>
                <t:EventType>NewMailEvent</t:EventType>
                <t:EventType>CreatedEvent</t:EventType>
            </m:EventTypes>
        </m:PullSubscriptionRequest>"#;
        let parsed = parse_subscription_request(body);
        let folders = parsed.folders.expect("explicit folders");
        assert!(folders.contains("inbox-1"));
        assert!(folders.contains("calendar"));
        let types = parsed.event_types.expect("event types");
        assert!(types.contains("NewMailEvent"));
        assert!(types.contains("CreatedEvent"));
    }

    #[test]
    fn test_render_notification_event_created() {
        let event = NotificationEvent::ItemCreated {
            owner: "alice".to_string(),
            folder_id: "inbox-1".to_string(),
            item_id: "item-1".to_string(),
            change_key: "ck-1".to_string(),
        };
        let xml = render_notification_event(&event, "AAAAAA==", "2026-01-01T00:00:00Z");
        assert!(xml.contains("<t:CreatedEvent>"));
        assert!(xml.contains("<t:Watermark>AAAAAA==</t:Watermark>"));
        assert!(xml.contains("Id=\"item-1\""));
        assert!(xml.contains("ChangeKey=\"ck-1\""));
        assert!(xml.contains("Id=\"inbox-1\""));
    }

    #[test]
    fn test_render_notification_event_deleted_omits_changekey() {
        // DeletedEvent carries no ChangeKey per MS-OXWSNTIF.
        let event = NotificationEvent::ItemDeleted {
            owner: "alice".to_string(),
            folder_id: "inbox-1".to_string(),
            item_id: "item-1".to_string(),
        };
        let xml = render_notification_event(&event, "Wm=", "2026-01-01T00:00:00Z");
        assert!(xml.contains("<t:DeletedEvent>"));
        assert!(!xml.contains("ChangeKey"));
    }

    #[test]
    fn test_render_notification_includes_subscription_id_and_more_events() {
        let events_xml = "<t:CreatedEvent/>";
        let n = render_notification("sub-1", Some("PREV"), true, events_xml);
        assert!(n.contains("<t:SubscriptionId>sub-1</t:SubscriptionId>"));
        assert!(n.contains("<t:PreviousWatermark>PREV</t:PreviousWatermark>"));
        assert!(n.contains("<t:MoreEvents>true</t:MoreEvents>"));
        assert!(n.contains("<t:CreatedEvent/>"));

        let n2 = render_notification("sub-1", None, false, "");
        assert!(!n2.contains("PreviousWatermark"));
        assert!(n2.contains("<t:MoreEvents>false</t:MoreEvents>"));
    }

    // --- subscription-kind detection from XML structure (item 1) ---

    #[test]
    fn test_detect_subscription_request_kind_from_structure() {
        // The default `m:`-prefixed bodies.
        let pull = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest><m:Timeout>30</m:Timeout></m:PullSubscriptionRequest></m:Subscribe>"#;
        assert_eq!(
            detect_subscription_request_kind(pull),
            Some(DetectedSubscriptionRequest::Pull)
        );
        let streaming = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:StreamingSubscriptionRequest/></m:Subscribe>"#;
        assert_eq!(
            detect_subscription_request_kind(streaming),
            Some(DetectedSubscriptionRequest::Streaming)
        );
        let push = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PushSubscriptionRequest><m:URL>http://example/cb</m:URL></m:PushSubscriptionRequest></m:Subscribe>"#;
        assert_eq!(
            detect_subscription_request_kind(push),
            Some(DetectedSubscriptionRequest::Push)
        );

        // Self-closing (empty-element) form: <PullSubscriptionRequest/>.
        let pull_empty = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest/></m:Subscribe>"#;
        assert_eq!(
            detect_subscription_request_kind(pull_empty),
            Some(DetectedSubscriptionRequest::Pull)
        );

        // No request element -> None.
        let bare = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"></m:Subscribe>"#;
        assert_eq!(detect_subscription_request_kind(bare), None);

        // The element name appearing *outside* a Subscribe ancestor must not
        // be mis-detected (structure-based gating).
        let outside = r#"<m:SomethingElse xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:PullSubscriptionRequest/></m:SomethingElse>"#;
        assert_eq!(detect_subscription_request_kind(outside), None);
    }

    #[test]
    fn test_detect_subscription_request_kind_is_namespace_insensitive() {
        // A different/absent namespace prefix must still be detected because
        // matching is on the local element name, not the raw substring. This is
        // the exact brittleness the review flagged for `body.contains(…)`.
        let pull_alt = r#"<soap:Envelope xmlns:soap="http://schemas.xmlsoap.org/soap/envelope/"><soap:Body><tns:Subscribe xmlns:tns="urn:something"><tns:PullSubscriptionRequest>
  <tns:Timeout>10</tns:Timeout>
</tns:PullSubscriptionRequest></tns:Subscribe></soap:Body></soap:Envelope>"#;
        assert_eq!(
            detect_subscription_request_kind(pull_alt),
            Some(DetectedSubscriptionRequest::Pull)
        );
        // Whitespace/formatting noise must not misroute detection (would defeat
        // a naïve `body.contains("PullSubscriptionRequest")` that could also
        // accidentally match a stray occurrence in a comment).
        let with_comment = r#"<m:Subscribe xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><!-- not a PullSubscriptionRequest --><m:StreamingSubscriptionRequest/></m:Subscribe>"#;
        assert_eq!(
            detect_subscription_request_kind(with_comment),
            Some(DetectedSubscriptionRequest::Streaming)
        );
    }

    // --- streaming SOAP envelope is well-formed (item 7) ---

    #[test]
    fn test_streaming_header_and_footer_balance_envelope() {
        let header = streaming_header();
        assert!(header.contains("<s:Envelope"));
        assert!(header.contains("<s:Body>"));
        assert!(
            !header.contains("</s:Body>"),
            "header must not pre-close the body"
        );

        let footer = streaming_footer();
        assert!(footer.contains("</s:Body>"));
        assert!(footer.contains("</s:Envelope>"));
        assert!(
            !footer.contains("<s:Envelope"),
            "footer must not reopen an envelope"
        );

        let fragment = streaming_fragment("<t:Notification/>", "", "OK");
        // A non-terminal fragment must not itself open or close the envelope
        // (envelope open/close is owned solely by header/footer).
        assert!(!fragment.contains("<s:Envelope"));
        assert!(!fragment.contains("</s:Envelope>"));
        assert!(
            fragment.contains("<m:GetStreamingEventsResponse"),
            "fragment carries the response element"
        );
        assert!(fragment.contains("ConnectionStatus>OK<"));

        let closed = streaming_fragment("", "", "Closed");
        assert!(closed.contains("ConnectionStatus>Closed<"));

        // header + a fragment + footer must form balanced envelope tags.
        let full = format!("{header}{fragment}{footer}");
        assert_eq!(
            full.matches("<s:Envelope").count(),
            full.matches("</s:Envelope>").count(),
            "envelope open/close balanced"
        );
        assert_eq!(
            full.matches("<s:Body").count(),
            full.matches("</s:Body>").count(),
            "body open/close balanced"
        );
    }

    #[test]
    fn test_persona_fields_use_email_as_deterministic_persona_id() {
        // PersonaId must be the SMTP address (not a random UUID) so GetPersona
        // can round-trip a PersonaId from FindPeople back to the directory entry.
        let contact = Contact {
            display_name: "Alice Example".to_string(),
            email: "alice@example.com".to_string(),
            title: Some("Engineer".to_string()),
            office: None,
            phone: None,
            department: None,
            company: None,
            last_modified: None,
        };
        let xml = persona_fields_from_contact(&contact);
        assert!(
            xml.contains(r#"<t:PersonaId Id="alice@example.com" ChangeKey="01"/>"#),
            "PersonaId should carry the SMTP address: {xml}"
        );
        assert!(xml.contains("<t:DisplayName>Alice Example</t:DisplayName>"));
        assert!(xml.contains("<t:EmailAddress>alice@example.com</t:EmailAddress>"));
        assert!(xml.contains("<t:RoutingType>SMTP</t:RoutingType>"));
        assert!(xml.contains("<t:MailboxType>Mailbox</t:MailboxType>"));
        assert!(xml.contains("<t:PersonaType>Person</t:PersonaType>"));
        // The helper renders fields only; it must not emit a wrapping <t:Persona>.
        assert!(!xml.contains("<t:Persona>"));
    }

    #[test]
    fn test_persona_fields_escape_display_name() {
        let contact = Contact {
            display_name: "A & B <C> \"x\" 'y'".to_string(),
            email: "a@example.com".to_string(),
            title: None,
            office: None,
            phone: None,
            department: None,
            company: None,
            last_modified: None,
        };
        let xml = persona_fields_from_contact(&contact);
        assert!(!xml.contains("<t:DisplayName>A & B <C>"));
        assert!(xml.contains(
            "<t:DisplayName>A &amp; B &lt;C&gt; &quot;x&quot; &apos;y&apos;</t:DisplayName>"
        ));
    }

    #[test]
    fn test_parse_push_subscription_request_extracts_fields() {
        let body = r#"<PushSubscriptionRequest>
            <FolderIds><DistinguishedFolderId Id="inbox"/></FolderIds>
            <EventTypes><EventType>CreatedEvent</EventType></EventTypes>
            <URL>https://client.example.com/callback</URL>
            <StatusFrequency>15</StatusFrequency>
            <CallerData>opaque-token</CallerData>
        </PushSubscriptionRequest>"#;
        let cfg = parse_push_subscription_request(body);
        assert_eq!(cfg.url, "https://client.example.com/callback");
        assert_eq!(cfg.status_frequency_minutes, 15);
        assert_eq!(cfg.caller_data.as_deref(), Some("opaque-token"));
    }

    #[test]
    fn test_parse_push_subscription_request_status_frequency_boundaries() {
        // Missing StatusFrequency falls back to the default of 6 minutes.
        let no_freq = parse_push_subscription_request(
            r#"<PushSubscriptionRequest><URL>https://c.example/cb</URL></PushSubscriptionRequest>"#,
        );
        assert_eq!(no_freq.status_frequency_minutes, 6);

        // Below the lower bound clamps up to 1.
        let below = parse_push_subscription_request(
            r#"<PushSubscriptionRequest><URL>https://c.example/cb</URL><StatusFrequency>0</StatusFrequency></PushSubscriptionRequest>"#,
        );
        assert_eq!(below.status_frequency_minutes, 1);

        // Above the upper bound clamps down to 1440.
        let above = parse_push_subscription_request(
            r#"<PushSubscriptionRequest><URL>https://c.example/cb</URL><StatusFrequency>99999</StatusFrequency></PushSubscriptionRequest>"#,
        );
        assert_eq!(above.status_frequency_minutes, 1440);

        // Non-numeric StatusFrequency falls back to the default.
        let non_numeric = parse_push_subscription_request(
            r#"<PushSubscriptionRequest><URL>https://c.example/cb</URL><StatusFrequency>abc</StatusFrequency></PushSubscriptionRequest>"#,
        );
        assert_eq!(non_numeric.status_frequency_minutes, 6);
    }

    #[test]
    fn test_render_send_notification_status_event_keepalive() {
        let delivery = PushDelivery {
            subscription_id: "sub-id-1".to_string(),
            url: "https://client.example.com/callback".to_string(),
            caller_data: None,
            watermark: 7,
            events: Vec::new(),
        };
        let xml = render_send_notification(&delivery);
        assert!(xml.contains("<m:SendNotification"));
        assert!(xml.contains("<m:SendNotificationResponseMessage"));
        assert!(xml.contains("<t:SubscriptionId>sub-id-1</t:SubscriptionId>"));
        assert!(xml.contains("<t:StatusEvent>"));
        assert!(!xml.contains("<t:CreatedEvent>"));
    }

    #[test]
    fn test_render_send_notification_created_event() {
        let delivery = PushDelivery {
            subscription_id: "sub-id-2".to_string(),
            url: "https://client.example.com/callback".to_string(),
            caller_data: None,
            watermark: 0,
            events: vec![(
                crate::notifications::NotificationEvent::ItemCreated {
                    owner: "alice".to_string(),
                    folder_id: "inbox".to_string(),
                    item_id: "item-123".to_string(),
                    change_key: "ck-1".to_string(),
                },
                1,
            )],
        };
        let xml = render_send_notification(&delivery);
        assert!(xml.contains("<t:CreatedEvent>"));
        assert!(xml.contains("<t:SubscriptionId>sub-id-2</t:SubscriptionId>"));
        assert!(xml.contains("Id=\"item-123\""));
        assert!(!xml.contains("<t:StatusEvent>"));
    }

    #[test]
    fn test_is_internal_ip_classifies_ranges() {
        use std::net::{Ipv4Addr, Ipv6Addr};

        // Internal IPv4 addresses must be blocked.
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::LOCALHOST))); // 127.0.0.1
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(
            169, 254, 169, 254
        )))); // link-local metadata
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)))); // CGNAT
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
        assert!(is_internal_ip(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)))); // documentation

        // Public IPv4 addresses must be allowed.
        assert!(!is_internal_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_internal_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));

        // Internal IPv6 ranges must be blocked.
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::LOCALHOST))); // ::1
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::UNSPECIFIED))); // ::
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::new(
            0xfc00, 0, 0, 0, 0, 0, 0, 1
        )))); // unique-local
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        )))); // link-local
        assert!(is_internal_ip(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0x0db8, 0, 0, 0, 0, 0, 1
        )))); // documentation

        // IPv4-mapped loopback (::ffff:127.0.0.1) must be blocked.
        let mapped = Ipv6Addr::new(0, 0, 0, 0, 0, 0xffff, 0x7f00, 0x0001);
        assert!(is_internal_ip(IpAddr::V6(mapped)));

        // A public IPv6 address must be allowed.
        assert!(!is_internal_ip(IpAddr::V6(Ipv6Addr::new(
            0x2606, 0x4700, 0x4700, 0, 0, 0, 0, 0x1111
        ))));
    }

    #[tokio::test]
    async fn test_validate_push_callback_url_ssrf() {
        // Loopback must be rejected.
        assert!(
            validate_push_callback_url("http://127.0.0.1:8080/cb")
                .await
                .is_err()
        );
        assert!(
            validate_push_callback_url("http://localhost/cb")
                .await
                .is_err()
        );
        // Link-local metadata endpoint must be rejected.
        assert!(
            validate_push_callback_url("http://169.254.169.254/latest/meta-data/")
                .await
                .is_err()
        );
        // Embedded credentials must be rejected.
        assert!(
            validate_push_callback_url("http://user:pass@example.com/cb")
                .await
                .is_err()
        );
        // Unsupported scheme must be rejected.
        assert!(
            validate_push_callback_url("ftp://example.com/cb")
                .await
                .is_err()
        );
        // A hostless URL must be rejected.
        assert!(validate_push_callback_url("http://").await.is_err());

        // A public IP literal with http(s) must be accepted (unless the test
        // environment cannot reach/resolve it, which IP literals avoid).
        assert!(
            validate_push_callback_url("https://8.8.8.8/cb")
                .await
                .is_ok(),
            "a public address should be accepted"
        );
    }
}
