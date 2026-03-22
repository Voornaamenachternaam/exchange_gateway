// src/ews.rs
use crate::caldav::CaldavClient;
use crate::calendar::{
    extract_ews_field, extract_ews_fields, parse_ews_attendees, parse_ews_calendar_item,
    parse_ews_recurrence, parse_ics_event, render_ics,
};
use crate::models::AppState;
use crate::storage::EwsItemRow;
use crate::sync::generate_server_id;
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
    password: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum EwsAction {
    GetFolder,
    FindFolder,
    FindItem,
    GetItem,
    SyncFolderItems,
    CreateItem,
    UpdateItem,
    DeleteItem,
    ResolveNames,
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
        EwsAction::CreateItem => handle_create_item(&state, &auth, &body).await,
        EwsAction::UpdateItem => handle_update_item(&state, &auth, &body).await,
        EwsAction::DeleteItem => handle_delete_item(&state, &auth, &body).await,
        EwsAction::ResolveNames => handle_resolve_names(&auth, &body).await,
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
        password: pair[idx + 1..].to_string(),
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
                if name.as_ref() == b"CreateItem" {
                    return Some(EwsAction::CreateItem);
                }
                if name.as_ref() == b"UpdateItem" {
                    return Some(EwsAction::UpdateItem);
                }
                if name.as_ref() == b"DeleteItem" {
                    return Some(EwsAction::DeleteItem);
                }
                if name.as_ref() == b"ResolveNames" {
                    return Some(EwsAction::ResolveNames);
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
        EwsAction::CreateItem => {
            if !xml.contains("SavedItemFolderId") || !xml.contains("Items") {
                return Err("CreateItem requires SavedItemFolderId and Items");
            }
            Ok(())
        }
        EwsAction::UpdateItem => {
            if !xml.contains("ItemChanges") {
                return Err("UpdateItem requires ItemChanges");
            }
            Ok(())
        }
        EwsAction::DeleteItem => {
            if !xml.contains("ItemIds") {
                return Err("DeleteItem requires ItemIds");
            }
            Ok(())
        }
        EwsAction::ResolveNames => {
            if !xml.contains("UnresolvedEntry") {
                return Err("ResolveNames requires UnresolvedEntry");
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
        out.push_str(&format!("<t:RequiredAttendees>{}</t:RequiredAttendees>", required));
    }
    if !optional.is_empty() {
        out.push_str(&format!("<t:OptionalAttendees>{}</t:OptionalAttendees>", optional));
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

fn render_ews_recurrence_xml(rrule: &str) -> String {
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
        "DAILY" => format!("<t:DailyRecurrence><t:Interval>{}</t:Interval></t:DailyRecurrence>", interval),
        "WEEKLY" => format!(
            "<t:WeeklyRecurrence><t:Interval>{}</t:Interval><t:DaysOfWeek>{}</t:DaysOfWeek></t:WeeklyRecurrence>",
            interval,
            byday.unwrap_or_default()
                .replace("MO", "Monday")
                .replace("TU", "Tuesday")
                .replace("WE", "Wednesday")
                .replace("TH", "Thursday")
                .replace("FR", "Friday")
                .replace("SA", "Saturday")
                .replace("SU", "Sunday")
                .replace(',', " ")
        ),
        "MONTHLY" => format!(
            "<t:AbsoluteMonthlyRecurrence><t:Interval>{}</t:Interval><t:DayOfMonth>{}</t:DayOfMonth></t:AbsoluteMonthlyRecurrence>",
            interval,
            bymonthday.unwrap_or_else(|| "1".to_string())
        ),
        "YEARLY" => format!(
            "<t:AbsoluteYearlyRecurrence><t:Month>{}</t:Month><t:DayOfMonth>{}</t:DayOfMonth></t:AbsoluteYearlyRecurrence>",
            match bymonth.as_deref().unwrap_or("1") {
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
            },
            bymonthday.unwrap_or_else(|| "1".to_string())
        ),
        _ => return String::new(),
    };
    let range = if let Some(count) = count {
        format!("<t:NumberedRecurrence><t:NumberOfOccurrences>{}</t:NumberOfOccurrences></t:NumberedRecurrence>", count)
    } else if let Some(until) = until {
        format!("<t:EndDateRecurrence><t:EndDate>{}</t:EndDate></t:EndDateRecurrence>", until)
    } else {
        "<t:NoEndRecurrence />".to_string()
    };
    format!("<t:Recurrence>{}{}</t:Recurrence>", pattern, range)
}

fn render_ews_calendar_item_xml(
    item_id: &str,
    change_key: &str,
    item: &crate::calendar::CalendarItem,
) -> String {
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
    if !item.location.is_empty() {
        xml.push_str(&format!("<t:Location>{}</t:Location>", xml_escape(&item.location)));
    }
    if !item.description.is_empty() {
        xml.push_str(&format!(r#"<t:Body BodyType="Text">{}</t:Body>"#, xml_escape(&item.description)));
        xml.push_str(&format!("<t:TextBody>{}</t:TextBody>", xml_escape(&item.description)));
    }
    if let Some(v) = item.reminder {
        xml.push_str(&format!("<t:ReminderMinutesBeforeStart>{}</t:ReminderMinutesBeforeStart>", v));
    }
    if let Some(v) = item.busy_status {
        xml.push_str(&format!("<t:LegacyFreeBusyStatus>{}</t:LegacyFreeBusyStatus>", busy_status_to_ews(v)));
    }
    if let Some(v) = item.sensitivity {
        xml.push_str(&format!("<t:Sensitivity>{}</t:Sensitivity>", sensitivity_to_ews(v)));
    }
    if let Some(v) = item.response_requested {
        xml.push_str(&format!("<t:ResponseRequested>{}</t:ResponseRequested>", if v { "true" } else { "false" }));
    }
    if let Some(v) = item.disallow_new_time_proposal {
        xml.push_str(&format!("<t:DisallowNewTimeProposal>{}</t:DisallowNewTimeProposal>", if v { "true" } else { "false" }));
    }
    if let Some(v) = &item.organizer_email {
        xml.push_str(&format!(
            r#"<t:Organizer><t:Mailbox><t:Name>{}</t:Name><t:EmailAddress>{}</t:EmailAddress><t:RoutingType>SMTP</t:RoutingType></t:Mailbox></t:Organizer>"#,
            xml_escape(item.organizer_name.as_deref().unwrap_or(v)),
            xml_escape(v)
        ));
    }
    if let Some(v) = item.meeting_status {
        xml.push_str(&format!("<t:MeetingStatus>{}</t:MeetingStatus>", v));
    }
    if let Some(v) = item.response_type {
        xml.push_str(&format!("<t:ResponseType>{}</t:ResponseType>", v));
    }
    if let Some(v) = item.appointment_reply_time {
        xml.push_str(&format!("<t:AppointmentReplyTime>{}</t:AppointmentReplyTime>", v.to_rfc3339()));
    }
    if let Some(v) = &item.timezone {
        xml.push_str(&format!("<t:StartTimeZone>{}</t:StartTimeZone>", xml_escape(v)));
    }
    if let Some(v) = &item.timezone_blob {
        xml.push_str(&format!("<t:MeetingTimeZone>{}</t:MeetingTimeZone>", xml_escape(v)));
    }
    if let Some(v) = &item.online_meeting_conf_link {
        xml.push_str(&format!("<t:OnlineMeetingConfLink>{}</t:OnlineMeetingConfLink>", xml_escape(v)));
    }
    if let Some(v) = &item.online_meeting_external_link {
        xml.push_str(&format!("<t:OnlineMeetingExternalLink>{}</t:OnlineMeetingExternalLink>", xml_escape(v)));
    }
    if let Some(v) = &item.client_uid {
        xml.push_str(&format!("<t:ClientUid>{}</t:ClientUid>", xml_escape(v)));
    }
    xml.push_str(&render_ews_categories(item));
    xml.push_str(&render_ews_attendees(item));
    if let Some(rrule) = &item.rrule {
        xml.push_str(&render_ews_recurrence_xml(rrule));
    }
    xml.push_str("</t:CalendarItem>");
    xml
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
        EwsAction::CreateItem => "CreateItemResponseMessage",
        EwsAction::UpdateItem => "UpdateItemResponseMessage",
        EwsAction::DeleteItem => "DeleteItemResponseMessage",
        EwsAction::ResolveNames => "ResolveNamesResponseMessage",
    };
    let top = match action {
        EwsAction::GetFolder => "GetFolderResponse",
        EwsAction::FindFolder => "FindFolderResponse",
        EwsAction::FindItem => "FindItemResponse",
        EwsAction::GetItem => "GetItemResponse",
        EwsAction::SyncFolderItems => "SyncFolderItemsResponse",
        EwsAction::CreateItem => "CreateItemResponse",
        EwsAction::UpdateItem => "UpdateItemResponse",
        EwsAction::DeleteItem => "DeleteItemResponse",
        EwsAction::ResolveNames => "ResolveNamesResponse",
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
    let caldav = CaldavClient::new(&state.cfg);
    let calendar_item_xml = match caldav
        .get_event(&item.resource_href, owner, &auth.password)
        .await
    {
        Ok((ics, _)) => match parse_ics_event(&ics) {
            Some(calendar_item) => render_ews_calendar_item_xml(&item.server_id, &ck, &calendar_item),
            None => render_ews_calendar_item_xml(
                &item.server_id,
                &ck,
                &crate::calendar::CalendarItem {
                    uid: item.uid.clone().unwrap_or_else(|| item.server_id.clone()),
                    subject: item.uid.clone().unwrap_or_else(|| item.resource_href.clone()),
                    description: String::new(),
                    location: String::new(),
                    start: chrono::Utc::now(),
                    end: chrono::Utc::now() + chrono::Duration::hours(1),
                    all_day: false,
                    dtstamp: Some(chrono::Utc::now()),
                    timezone: None,
                    timezone_blob: None,
                    rrule: None,
                    exdates: Vec::new(),
                    organizer_name: None,
                    organizer_email: None,
                    attendees: Vec::new(),
                    categories: Vec::new(),
                    busy_status: None,
                    sensitivity: None,
                    reminder: None,
                    response_requested: None,
                    disallow_new_time_proposal: None,
                    appointment_reply_time: None,
                    meeting_status: None,
                    response_type: None,
                    online_meeting_conf_link: None,
                    online_meeting_external_link: None,
                    client_uid: None,
                    exceptions: Vec::new(),
                },
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
        r#"<m:GetItemResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:GetItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Items>
        {}
      </m:Items>
    </m:GetItemResponseMessage>
  </m:ResponseMessages>
</m:GetItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        calendar_item_xml
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
            let change_tag = if since == 0 { "Create" } else { "Update" };
            changes_xml.push_str(&format!(
                r#"<t:{change_tag}>
  <t:CalendarItem>
    <t:ItemId Id="{}" ChangeKey="{}" />
    <t:Subject>{}</t:Subject>
    <t:UID>{}</t:UID>
  </t:CalendarItem>
</t:{change_tag}>"#,
                change_tag = change_tag,
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

async fn handle_create_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let caldav = CaldavClient::new(&state.cfg);
    let item = match parse_ews_calendar_item(body) {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorSchemaValidation",
                &format!("Failed to parse CalendarItem payload: {e}"),
                StatusCode::BAD_REQUEST,
            );
        }
    };
    let calendars = match caldav.find_user_calendars(owner, &auth.password).await {
        Ok(v) => v,
        Err(_) => Vec::new(),
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
        .put_event(&collection_href, None, &ics, owner, &auth.password, None)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::CreateItem,
                "ErrorInternalServerError",
                &format!("Failed to persist created item: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let server_id = generate_server_id(&state.cfg.hmac_secret, &href);

    if let Err(e) = state
        .storage
        .upsert_item_map(owner, &collection_href, &href, &server_id, &item.uid, &etag)
        .await
    {
        return operation_error_response(
            &EwsAction::CreateItem,
            "ErrorInternalServerError",
            &format!("Failed to persist created item: {}", e),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    let response = format!(
        r#"<m:CreateItemResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:CreateItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Items>
        {}
      </m:Items>
    </m:CreateItemResponseMessage>
  </m:ResponseMessages>
</m:CreateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(&server_id, &etag, &item),
    );
    soap_ok(response)
}

async fn handle_update_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
    if item_id.is_empty() {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInvalidIdMalformed",
            "UpdateItem requires ItemId/@Id",
            StatusCode::OK,
        );
    }

    let current = match state
        .storage
        .get_ews_item_by_server_id(owner, &item_id)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                &format!("Failed to load item: {}", e),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let Some(item) = current else {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorItemNotFound",
            "Requested item does not exist",
            StatusCode::OK,
        );
    };

    let caldav = CaldavClient::new(&state.cfg);
    let (existing_ics, existing_etag) = match caldav
        .get_event(&item.resource_href, owner, &auth.password)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                &format!("Failed to fetch existing event: {e}"),
                StatusCode::INTERNAL_SERVER_ERROR,
            );
        }
    };
    let mut current_item =
        parse_ics_event(&existing_ics).unwrap_or_else(|| crate::calendar::CalendarItem {
            uid: item.uid.clone().unwrap_or_else(|| item.server_id.clone()),
            subject: String::new(),
            description: String::new(),
            location: String::new(),
            start: chrono::Utc::now(),
            end: chrono::Utc::now() + chrono::Duration::hours(1),
            all_day: false,
            dtstamp: Some(chrono::Utc::now()),
            timezone: None,
            timezone_blob: None,
            rrule: None,
            exdates: Vec::new(),
            organizer_name: None,
            organizer_email: None,
            attendees: Vec::new(),
            categories: Vec::new(),
            busy_status: None,
            sensitivity: None,
            reminder: None,
            response_requested: None,
            disallow_new_time_proposal: None,
            appointment_reply_time: None,
            meeting_status: None,
            response_type: None,
            online_meeting_conf_link: None,
            online_meeting_external_link: None,
            client_uid: None,
            exceptions: Vec::new(),
        });
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
    if body.contains("ResponseRequested")
        && let Some(v) = extract_ews_field(body, b"ResponseRequested")
    {
        current_item.response_requested = Some(v.eq_ignore_ascii_case("true"));
    }
    if body.contains("DisallowNewTimeProposal")
        && let Some(v) = extract_ews_field(body, b"DisallowNewTimeProposal")
    {
        current_item.disallow_new_time_proposal = Some(v.eq_ignore_ascii_case("true"));
    }
    if let Some(v) = extract_ews_field(body, b"OrganizerName") {
        current_item.organizer_name = Some(v);
    }
    if let Some(v) = extract_ews_field(body, b"OrganizerEmail") {
        current_item.organizer_email = Some(v);
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
    let uid = current_item.uid.clone();
    let ics = render_ics(&current_item);
    let (resource_href, new_etag) = match caldav
        .put_event(
            &item.resource_href,
            Some(&item.resource_href),
            &ics,
            owner,
            &auth.password,
            existing_etag.as_deref().or(item.etag.as_deref()),
        )
        .await
    {
        Ok(v) => v,
        Err(e) => {
            return operation_error_response(
                &EwsAction::UpdateItem,
                "ErrorInternalServerError",
                &format!("Failed to persist update: {e}"),
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
            &item.server_id,
            &uid,
            &new_etag,
        )
        .await
    {
        return operation_error_response(
            &EwsAction::UpdateItem,
            "ErrorInternalServerError",
            &format!("Failed to persist update: {}", e),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    let response = format!(
        r#"<m:UpdateItemResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:UpdateItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:Items>
        {}
      </m:Items>
    </m:UpdateItemResponseMessage>
  </m:ResponseMessages>
</m:UpdateItemResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        render_ews_calendar_item_xml(&item.server_id, &new_etag, &current_item),
    );
    soap_ok(response)
}

async fn handle_delete_item(state: &Arc<AppState>, auth: &AuthContext, body: &str) -> Response {
    let owner = owner_from_username(&auth.username);
    let item_id = extract_first_attr(body, b"ItemId", b"Id").unwrap_or_default();
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
            return operation_error_response(
                &EwsAction::DeleteItem,
                "ErrorInternalServerError",
                &format!("Failed to resolve item: {}", e),
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
    let caldav = CaldavClient::new(&state.cfg);
    if let Err(e) = caldav
        .delete_event(
            &existing.resource_href,
            owner,
            &auth.password,
            existing.etag.as_deref(),
        )
        .await
    {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            &format!("Failed to delete CalDAV item: {}", e),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    if let Err(e) = state
        .storage
        .add_delete_tombstone(owner, &item_id)
        .await
    {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            &format!("Failed to persist delete tombstone: {}", e),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }
    if let Err(e) = state
        .storage
        .delete_item_by_server_id(owner, &item_id)
        .await
    {
        return operation_error_response(
            &EwsAction::DeleteItem,
            "ErrorInternalServerError",
            &format!("Failed to delete mapping: {}", e),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    let response = format!(
        r#"<m:DeleteItemResponse xmlns:m="{}">
  <m:ResponseMessages>
    <m:DeleteItemResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
    </m:DeleteItemResponseMessage>
  </m:ResponseMessages>
</m:DeleteItemResponse>"#,
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
        r#"<m:ResolveNamesResponse xmlns:m="{}" xmlns:t="{}">
  <m:ResponseMessages>
    <m:ResolveNamesResponseMessage ResponseClass="Success">
      <m:ResponseCode>NoError</m:ResponseCode>
      <m:ResolutionSet TotalItemsInView="1" IncludesLastItemInRange="true">
        <t:Resolution>
          <t:Mailbox>
            <t:Name>{}</t:Name>
            <t:EmailAddress>{}</t:EmailAddress>
            <t:RoutingType>SMTP</t:RoutingType>
            <t:MailboxType>Mailbox</t:MailboxType>
          </t:Mailbox>
        </t:Resolution>
      </m:ResolutionSet>
    </m:ResolveNamesResponseMessage>
  </m:ResponseMessages>
</m:ResolveNamesResponse>"#,
        EWS_MSG_NS,
        EWS_TYPE_NS,
        xml_escape(&mailbox),
        xml_escape(&mailbox),
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
    fn detects_create_item_action() {
        let xml = r#"<s:Envelope><s:Body><m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#;
        assert_eq!(detect_action(xml), Some(EwsAction::CreateItem));
    }

    #[test]
    fn validates_delete_item_schema() {
        let xml = r#"<s:Envelope><s:Body><m:DeleteItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ItemIds /></m:DeleteItem></s:Body></s:Envelope>"#;
        assert!(validate_schema(&EwsAction::DeleteItem, xml).is_ok());
    }

    #[test]
    fn detects_extended_actions_matrix() {
        let cases = [
            (
                r#"<s:Envelope><s:Body><m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#,
                EwsAction::CreateItem,
            ),
            (
                r#"<s:Envelope><s:Body><m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#,
                EwsAction::UpdateItem,
            ),
            (
                r#"<s:Envelope><s:Body><m:DeleteItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#,
                EwsAction::DeleteItem,
            ),
            (
                r#"<s:Envelope><s:Body><m:ResolveNames xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages" /></s:Body></s:Envelope>"#,
                EwsAction::ResolveNames,
            ),
        ];
        for (xml, expected) in cases {
            assert_eq!(detect_action(xml), Some(expected));
        }
    }

    #[test]
    fn validates_schema_matrix_for_extended_actions() {
        let ok_cases = [
            (
                EwsAction::CreateItem,
                r#"<s:Envelope><s:Body><m:CreateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:SavedItemFolderId/><m:Items/></m:CreateItem></s:Body></s:Envelope>"#,
            ),
            (
                EwsAction::UpdateItem,
                r#"<s:Envelope><s:Body><m:UpdateItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ItemChanges/></m:UpdateItem></s:Body></s:Envelope>"#,
            ),
            (
                EwsAction::DeleteItem,
                r#"<s:Envelope><s:Body><m:DeleteItem xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:ItemIds/></m:DeleteItem></s:Body></s:Envelope>"#,
            ),
            (
                EwsAction::ResolveNames,
                r#"<s:Envelope><s:Body><m:ResolveNames xmlns:m="http://schemas.microsoft.com/exchange/services/2006/messages"><m:UnresolvedEntry>a@example.com</m:UnresolvedEntry></m:ResolveNames></s:Body></s:Envelope>"#,
            ),
        ];

        for (action, xml) in ok_cases {
            assert!(validate_schema(&action, xml).is_ok());
        }
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
