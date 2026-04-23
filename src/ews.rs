// src/ews.rs
use crate::attachment::{
    parse_create_attachment_request, parse_delete_attachment_request, parse_get_attachment_request,
    render_create_attachment_response,
    render_file_attachment_xml, render_get_attachment_response,
};
use crate::caldav::CaldavClient;
use crate::calendar::{
    extract_ews_field, extract_ews_fields, parse_ews_attendees, parse_ews_calendar_item,
    parse_ews_recurrence, parse_ics_event, render_ics,
};
use crate::delegate_ews::DelegateEwsHandler;
use crate::ews_folders::{
    DistinguishedFolder, folder_id_for, render_folder_xml, validate_folder_request,
};
use crate::ews_update::{apply_field_changes, parse_item_changes};
use crate::models::AppState;
use crate::permission::{PermissionContext, PermissionEnforcement};
use crate::protocol_fixtures::{EWS_MSG_NS, EWS_TYPE_NS};
use crate::room::{
    parse_get_rooms_request, render_get_room_lists_response, render_get_rooms_response,
};
use crate::storage::EwsItemRow;
use crate::sync::generate_server_id;
use crate::util::nfc;
use crate::util::xml_escape;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Datelike;
use const_hex;
use quick_xml::Reader;
use quick_xml::events::Event;
use secrecy::{ExposeSecret, SecretString};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

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

#[non_exhaustive]
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
}

impl EwsAction {
    #[must_use]
    const fn requires_mime_validation(&self) -> bool {
        matches!(self, EwsAction::FindItem | EwsAction::SyncFolderItems)
    }

    #[must_use]
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

    if action.requires_mime_validation() && xml.contains("IncludeMimeContent") {
        return Err("This operation does not support IncludeMimeContent");
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
        EwsAction::ResolveNames => handle_resolve_names(&auth, &body).await,
        EwsAction::GetUserOofSettings => handle_get_user_oof_settings(&auth, &body).await,
        EwsAction::SetUserOofSettings => handle_set_user_oof_settings(&auth, &body).await,
        EwsAction::GetServiceConfiguration => handle_get_service_configuration().await,
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
    let user = creds[..idx].to_string();
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
    if let Some(u) = &item.updated_at {
        h.update(u.as_bytes());
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
                s.to_rfc3339()
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
            start.to_rfc3339(), end.to_rfc3339(), e.exception_start.to_rfc3339(), xml_escape(subject)
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
        item.start.to_rfc3339(),
        item.end.to_rfc3339(),
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
        created.to_rfc3339(), created.to_rfc3339(), created.to_rfc3339(), created.to_rfc3339()
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
        "<t:IsMeeting>{}</t:IsMeeting><t:IsOrganizer>{}</t:IsOrganizer><t:IsRecurring>{}</t:IsRecurring><t:IsCancelled>{}</t:IsCancelled><t:HasAttachments>false</t:HasAttachments>",
        if is_meeting { "true" } else { "false" }, if is_organizer { "true" } else { "false" },
        if item.rrule.is_some() { "true" } else { "false" }, if is_cancelled { "true" } else { "false" }
    ));
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
            v.to_rfc3339()
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
            item.dtstamp.unwrap_or_else(chrono::Utc::now).to_rfc3339()
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
        item.dtstamp.unwrap_or_else(chrono::Utc::now).to_rfc3339()
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
    render_ews_calendar_item_xml_with_shape(item_id, change_key, item, ItemShape::AllProperties)
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
        reader.config_mut().trim_text(true);
        let mut buf = Vec::new();
        let mut in_calendar_data = false;
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = true;
                }
                Ok(Event::Text(t)) if in_calendar_data => {
                    if let Ok(ics) = t.decode()
                        && let Some(item) = parse_ics_event(&ics)
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
                            item.start.to_rfc3339(), item.end.to_rfc3339(), busy_type, ews_calendar_event_details_xml(&item)
                        ));
                    }
                }
                Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                    in_calendar_data = false;
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
        entry.push(format!("<t:Suggestion><t:MeetingTime>{}</t:MeetingTime><t:IsWorkTime>true</t:IsWorkTime><t:SuggestionQuality>Excellent</t:SuggestionQuality></t:Suggestion>", slot_start.to_rfc3339()));
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
    let len = a.len().max(b.len());
    let mut merged = String::with_capacity(len);
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    for i in 0..len {
        let l = *ab.get(i).unwrap_or(&b'0');
        let r = *bb.get(i).unwrap_or(&b'0');
        merged.push(char::from(l.max(r)));
    }
    merged
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
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut href = String::new();
    let mut etag = String::new();
    let mut ics = String::new();
    let mut out = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                b"href" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        href = t.decode().unwrap_or_default().to_string();
                    }
                }
                b"getetag" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        etag = t.decode().unwrap_or_default().trim_matches('"').to_string();
                    }
                }
                b"calendar-data" => {
                    if let Ok(Event::Text(t)) = reader.read_event_into(&mut buf) {
                        ics = t.decode().unwrap_or_default().to_string();
                    }
                }
                _ => {}
            },
            Ok(Event::End(ref e)) if e.name().local_name().as_ref() == b"response" => {
                if !href.is_empty()
                    && let Some(item) = parse_ics_event(&ics)
                {
                    let server_id = generate_server_id(state.cfg.hmac_secret(), &href);
                    let safe_etag = if etag.is_empty() {
                        const_hex::encode({
                            let mut h = Sha256::new();
                            h.update(server_id.as_bytes());
                            h.finalize()
                        })
                    } else {
                        etag.clone()
                    };
                    let _ = state
                        .storage
                        .upsert_item_map(
                            owner,
                            &collection_href,
                            &href,
                            &server_id,
                            &item.uid,
                            &safe_etag,
                        )
                        .await;
                    out.push(CurrentCalendarItem {
                        row: EwsItemRow {
                            server_id,
                            resource_href: href.clone(),
                            uid: Some(item.uid.clone()),
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
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

async fn handle_get_folder(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::GetFolder, owner, body) {
        return *resp;
    }
    let distinguished_str = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let folder =
        DistinguishedFolder::from_str(&distinguished_str).unwrap_or(DistinguishedFolder::Calendar);
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
    let distinguished_str = extract_first_attr(body, b"DistinguishedFolderId", b"Id")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (total_count, cal_xml) = match distinguished_str.as_str() {
        "msgfolderroot" | "" => {
            let count =
                load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
                    .await
                    .map(|v| v.len())
                    .unwrap_or(0);
            (
                1usize,
                render_folder_xml(owner, DistinguishedFolder::Calendar, count),
            )
        }
        _ => (0usize, String::new()),
    };
    let response = format!(
        r#"<m:FindFolderResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindFolderResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="true"><t:Folders>{}</t:Folders></m:RootFolder></m:FindFolderResponseMessage></m:ResponseMessages></m:FindFolderResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, total_count, cal_xml
    );
    soap_ok(response)
}

async fn handle_find_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::FindItem, owner, body) {
        return *resp;
    }
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
        item_xml.push_str(&render_ews_calendar_item_xml_with_shape(
            &current.row.server_id,
            &ck,
            &current.item,
            shape,
        ));
    }
    let includes_last = if offset + paged.len() >= total_items {
        "true"
    } else {
        "false"
    };
    let next_offset = offset + paged.len();
    let _ = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &format!("offset:{}", next_offset))
        .await;
    let response = format!(
        r#"<m:FindItemResponse xmlns:m="{}" xmlns:t="{}"><m:ResponseMessages><m:FindItemResponseMessage ResponseClass="Success"><m:ResponseCode>NoError</m:ResponseCode><m:RootFolder TotalItemsInView="{}" IncludesLastItemInRange="{}" IndexedPagingOffset="{}"><t:Items>{}</t:Items></m:RootFolder></m:FindItemResponseMessage></m:ResponseMessages></m:FindItemResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS, total_items, includes_last, next_offset, item_xml
    );
    soap_ok(response)
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

    let owner = match state.storage.get_ews_item_owner(&item_id).await {
        Ok(Some(o)) => o,
        Ok(None) => {
            return operation_error_response(
                &EwsAction::GetItem,
                "ErrorItemNotFound",
                "Requested item does not exist",
                StatusCode::OK,
            );
        }
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

    let calendar_folder_id = folder_id_for(&owner, DistinguishedFolder::Calendar);
    let enforcement = PermissionEnforcement::new(&state.storage);
    let perm_ctx = PermissionContext::new(
        auth.username.clone(),
        owner.clone(),
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
        .get_ews_item_by_server_id(&owner, &item_id)
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
        .get_event(&item.resource_href, &owner, auth.password.expose_secret())
        .await
    {
        Ok((ics, _)) => match parse_ics_event(&ics) {
            Some(ci) => render_ews_calendar_item_xml_with_shape(&item.server_id, &ck, &ci, shape),
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

async fn handle_sync_folder_items(
    state: &Arc<AppState>,
    auth: &AuthContext,
    body: &str,
) -> Response {
    let owner = owner_from_username(&auth.username);
    if let Err(resp) = validate_requested_folder(&EwsAction::SyncFolderItems, owner, body) {
        return *resp;
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
    let journal_rows = match state
        .storage
        .list_journal_since_seq(owner, since, upper_bound, max_changes.saturating_add(1))
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
    let has_more = journal_rows.len() > max_changes;
    let visible_rows = if has_more {
        &journal_rows[..max_changes]
    } else {
        &journal_rows[..]
    };
    let mut emitted_ids = HashSet::new();
    let mut changes_xml = String::new();
    let mut last_returned_seq = since;
    for row in visible_rows {
        last_returned_seq = row.seq;
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
            let change_tag = if since == 0 { "Create" } else { "Update" };
            changes_xml.push_str(&format!(
                r#"<t:{ct}>{}</t:{ct}>"#,
                render_ews_calendar_item_xml_with_shape(
                    &item.row.server_id,
                    &ck,
                    &item.item,
                    shape
                ),
                ct = change_tag
            ));
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
    let _ = state
        .storage
        .set_ews_sync_state(owner, &folder_id, &new_sync_state)
        .await;
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
    let _ = state
        .storage
        .set_ews_sync_state(owner, &sync_state_key, &new_sync_state)
        .await;
    let changes = if is_initial {
        let count = load_current_calendar_items(state, owner, auth.password.expose_secret(), None)
            .await
            .map(|v| v.len())
            .unwrap_or(0);
        let cal_xml = render_folder_xml(owner, DistinguishedFolder::Calendar, count);
        format!("<t:Create>{}</t:Create>", cal_xml)
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

async fn handle_update_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
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
    if let Err(resp) = validate_item_change_key(&EwsAction::UpdateItem, body, &stored_item) {
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
    let uid = current_item.uid.clone();
    let ics = render_ics(&current_item);
    let (resource_href, new_etag) = match caldav
        .put_event(
            &stored_item.resource_href,
            Some(&stored_item.resource_href),
            &ics,
            owner,
            auth.password.expose_secret(),
            existing_etag.as_deref().or(stored_item.etag.as_deref()),
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
            &resource_href,
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

async fn handle_delete_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
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
    if let Err(e) = caldav
        .delete_event(
            &existing.resource_href,
            owner,
            auth.password.expose_secret(),
            existing.etag.as_deref(),
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

async fn handle_get_service_configuration() -> Response {
    let inner = format!(
        r#"<m:GetServiceConfigurationResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetServiceConfigurationResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
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
      <m:TimeZoneDefinitions/>
    </m:GetServerTimeZonesResponseMessage>
  </m:ResponseMessages>
</m:GetServerTimeZonesResponse>"#,
        EWS_MSG_NS, EWS_TYPE_NS
    );
    soap_ok(inner)
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
        room_manager.get_rooms_for_list(&owner, &room_list_email).await
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
                    r#"<t:FileAttachment>
                        <t:AttachmentId Id="{}"/>
                        <t:Name>attachment.dat</t:Name>
                        <t:ContentType>application/octet-stream</t:ContentType>
                        <t:Size>0</t:Size>
                        <t:IsInline>false</t:IsInline>
                    </t:FileAttachment>"#,
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
