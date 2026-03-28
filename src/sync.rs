// src/sync.rs
use crate::caldav::CaldavClient;
use crate::calendar::{
    parse_eas_sync_mutations, parse_ics_event, render_ics, Attendee, CalendarException,
    CalendarItem, EasSyncMutation,
};
use crate::models::AppState;
use anyhow::{anyhow, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{TimeZone, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const INVALID_SYNC_KEY_STATUS: &str = "9";

#[derive(Clone, Debug)]
pub enum ClientMutationResult {
    Add {
        client_id: Option<String>,
        server_id: Option<String>,
        status: &'static str,
    },
    Change {
        server_id: String,
        status: &'static str,
    },
    Delete {
        server_id: String,
        status: &'static str,
    },
}

pub fn generate_server_id(secret: &str, resource_href: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC init");
    mac.update(resource_href.as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn sync_seq_to_token(seq: i64) -> String {
    format!("seq:{}", seq.max(0))
}

fn sync_since_from_token(token: Option<&str>) -> i64 {
    token
        .and_then(|raw| raw.strip_prefix("seq:"))
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
}

pub fn invalid_sync_key_response(collection_id: &str, content_class: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:">
  <Collections>
    <Collection>
      <Class>{}</Class>
      <SyncKey>0</SyncKey>
      <CollectionId>{}</CollectionId>
      <Status>{}</Status>
    </Collection>
  </Collections>
</Sync>"#,
        xml_escape(content_class),
        xml_escape(collection_id),
        INVALID_SYNC_KEY_STATUS
    )
}

pub async fn apply_client_sync_mutations(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    username: &str,
    password: &str,
    xml: &str,
) -> Result<Vec<ClientMutationResult>> {
    let mutations = parse_eas_sync_mutations(xml)?;
    if mutations.is_empty() {
        return Ok(Vec::new());
    }

    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow!("no calendars found"))?
        .clone();

    let mut results = Vec::new();

    for mutation in mutations {
        match mutation {
            EasSyncMutation::Add { client_id, item } => {
                if let Some(client_id) = client_id.as_deref()
                    && let Some((server_id, status)) = state
                        .storage
                        .get_client_sync_command(owner, collection_id, client_id)
                        .await?
                {
                    results.push(ClientMutationResult::Add {
                        client_id: Some(client_id.to_string()),
                        server_id,
                        status: if status == "1" { "1" } else { "6" },
                    });
                    continue;
                }

                let mut item = item;
                if item.uid.is_empty() {
                    item.uid = client_id
                        .clone()
                        .unwrap_or_else(|| Uuid::new_v4().to_string());
                }

                let ics = render_ics(&item);
                match caldav
                    .put_event(&collection_href, None, &ics, username, password, None)
                    .await
                {
                    Ok((resource_href, etag)) => {
                        let server_id = generate_server_id(&state.cfg.hmac_secret, &resource_href);
                        state
                            .storage
                            .upsert_item_map(
                                owner,
                                &collection_href,
                                &resource_href,
                                &server_id,
                                &item.uid,
                                &etag,
                            )
                            .await?;

                        if let Some(client_id) = client_id.as_deref() {
                            state
                                .storage
                                .put_client_sync_command(
                                    owner,
                                    collection_id,
                                    client_id,
                                    Some(&server_id),
                                    "1",
                                )
                                .await?;
                        }

                        results.push(ClientMutationResult::Add {
                            client_id,
                            server_id: Some(server_id),
                            status: "1",
                        });
                    }
                    Err(_) => {
                        if let Some(client_id) = client_id.as_deref() {
                            let _ = state
                                .storage
                                .put_client_sync_command(owner, collection_id, client_id, None, "6")
                                .await;
                        }
                        results.push(ClientMutationResult::Add {
                            client_id,
                            server_id: None,
                            status: "6",
                        });
                    }
                }
            }

            EasSyncMutation::Change { server_id, patch } => {
                let Some(existing) = state
                    .storage
                    .get_ews_item_by_server_id(owner, &server_id)
                    .await?
                else {
                    results.push(ClientMutationResult::Change {
                        server_id,
                        status: "6",
                    });
                    continue;
                };

                let (existing_ics, existing_etag) = caldav
                    .get_event(&existing.resource_href, username, password)
                    .await?;
                let mut item = parse_ics_event(&existing_ics)
                    .ok_or_else(|| anyhow!("failed parsing existing calendar item"))?;

                if let Some(v) = patch.uid {
                    item.uid = v;
                }
                if let Some(v) = patch.subject {
                    item.subject = v;
                }
                if let Some(v) = patch.description {
                    item.description = v;
                }
                if let Some(v) = patch.location {
                    item.location = v;
                }
                if let Some(v) = patch.start {
                    item.start = v;
                }
                if let Some(v) = patch.end {
                    item.end = v;
                }
                if let Some(v) = patch.all_day {
                    item.all_day = v;
                }
                if let Some(v) = patch.dtstamp {
                    item.dtstamp = Some(v);
                }
                if let Some(v) = patch.timezone {
                    item.timezone = Some(v);
                }
                if let Some(v) = patch.timezone_blob {
                    item.timezone_blob = Some(v);
                }
                if let Some(v) = patch.rrule {
                    item.rrule = Some(v);
                }
                if let Some(v) = patch.exdates {
                    item.exdates = v;
                }
                if let Some(v) = patch.organizer_name {
                    item.organizer_name = Some(v);
                }
                if let Some(v) = patch.organizer_email {
                    item.organizer_email = Some(v);
                }
                if let Some(v) = patch.attendees {
                    item.attendees = v;
                }
                if let Some(v) = patch.categories {
                    item.categories = v;
                }
                if let Some(v) = patch.busy_status {
                    item.busy_status = Some(v);
                }
                if let Some(v) = patch.sensitivity {
                    item.sensitivity = Some(v);
                }
                if let Some(v) = patch.reminder {
                    item.reminder = Some(v);
                }
                if let Some(v) = patch.response_requested {
                    item.response_requested = Some(v);
                }
                if let Some(v) = patch.disallow_new_time_proposal {
                    item.disallow_new_time_proposal = Some(v);
                }
                if let Some(v) = patch.appointment_reply_time {
                    item.appointment_reply_time = Some(v);
                }
                if let Some(v) = patch.meeting_status {
                    item.meeting_status = Some(v);
                }
                if let Some(v) = patch.response_type {
                    item.response_type = Some(v);
                }
                if let Some(v) = patch.online_meeting_conf_link {
                    item.online_meeting_conf_link = Some(v);
                }
                if let Some(v) = patch.online_meeting_external_link {
                    item.online_meeting_external_link = Some(v);
                }
                if let Some(v) = patch.client_uid {
                    item.client_uid = Some(v);
                }
                if let Some(v) = patch.exceptions {
                    item.exceptions = v;
                }

                let ics = render_ics(&item);
                match caldav
                    .put_event(
                        &existing.resource_href,
                        Some(&existing.resource_href),
                        &ics,
                        username,
                        password,
                        existing_etag.as_deref().or(existing.etag.as_deref()),
                    )
                    .await
                {
                    Ok((resource_href, etag)) => {
                        state
                            .storage
                            .upsert_item_map(
                                owner,
                                &existing.resource_href,
                                &resource_href,
                                &server_id,
                                &item.uid,
                                &etag,
                            )
                            .await?;
                        results.push(ClientMutationResult::Change {
                            server_id,
                            status: "1",
                        });
                    }
                    Err(_) => results.push(ClientMutationResult::Change {
                        server_id,
                        status: "6",
                    }),
                }
            }

            EasSyncMutation::Delete { server_id } => {
                let Some(existing) = state
                    .storage
                    .get_ews_item_by_server_id(owner, &server_id)
                    .await?
                else {
                    results.push(ClientMutationResult::Delete {
                        server_id,
                        status: "6",
                    });
                    continue;
                };

                match caldav
                    .delete_event(
                        &existing.resource_href,
                        username,
                        password,
                        existing.etag.as_deref(),
                    )
                    .await
                {
                    Ok(()) => {
                        state.storage.add_delete_tombstone(owner, &server_id).await?;
                        state
                            .storage
                            .delete_item_by_server_id(owner, &server_id)
                            .await?;
                        results.push(ClientMutationResult::Delete {
                            server_id,
                            status: "1",
                        });
                    }
                    Err(_) => results.push(ClientMutationResult::Delete {
                        server_id,
                        status: "6",
                    }),
                }
            }
        }
    }

    Ok(results)
}

pub fn render_client_mutation_responses(results: &[ClientMutationResult]) -> String {
    if results.is_empty() {
        return String::new();
    }

    let mut xml = String::from("<Responses>");
    for result in results {
        match result {
            ClientMutationResult::Add {
                client_id,
                server_id,
                status,
            } => {
                xml.push_str("<Add>");
                if let Some(client_id) = client_id {
                    xml.push_str(&format!("<ClientId>{}</ClientId>", xml_escape(client_id)));
                }
                if let Some(server_id) = server_id {
                    xml.push_str(&format!("<ServerId>{}</ServerId>", xml_escape(server_id)));
                }
                xml.push_str(&format!("<Status>{}</Status></Add>", status));
            }
            ClientMutationResult::Change { server_id, status } => {
                xml.push_str(&format!(
                    "<Change><ServerId>{}</ServerId><Status>{}</Status></Change>",
                    xml_escape(server_id),
                    status
                ));
            }
            ClientMutationResult::Delete { server_id, status } => {
                xml.push_str(&format!(
                    "<Delete><ServerId>{}</ServerId><Status>{}</Status></Delete>",
                    xml_escape(server_id),
                    status
                ));
            }
        }
    }
    xml.push_str("</Responses>");
    xml
}

pub async fn apply_meeting_response(
    state: Arc<AppState>,
    owner: &str,
    username: &str,
    password: &str,
    request_id: &str,
    user_response: u8,
) -> Result<()> {
    let Some(existing) = state
        .storage
        .get_ews_item_by_server_id(owner, request_id)
        .await?
    else {
        return Err(anyhow!("unknown meeting request id: {request_id}"));
    };

    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow!("no calendars found"))?
        .clone();

    let (existing_ics, existing_etag) = caldav
        .get_event(&existing.resource_href, username, password)
        .await?;
    let mut item =
        parse_ics_event(&existing_ics).ok_or_else(|| anyhow!("failed parsing existing event"))?;

    let (status, partstat) = match user_response {
        1 => (3, "ACCEPTED"),
        2 => (2, "TENTATIVE"),
        3 => (4, "DECLINED"),
        _ => (5, "NEEDS-ACTION"),
    };

    if let Some(attendee) = item
        .attendees
        .iter_mut()
        .find(|a| a.email.eq_ignore_ascii_case(owner))
    {
        attendee.attendee_status = Some(status);
        attendee.partstat = Some(partstat.to_string());
    } else {
        item.attendees.push(Attendee {
            name: None,
            email: owner.to_string(),
            attendee_type: Some(1),
            attendee_status: Some(status),
            partstat: Some(partstat.to_string()),
        });
    }

    item.response_type = Some(status);
    item.appointment_reply_time = Some(Utc::now());

    let ics = render_ics(&item);
    let (resource_href, etag) = caldav
        .put_event(
            &collection_href,
            Some(&existing.resource_href),
            &ics,
            username,
            password,
            existing_etag.as_deref().or(existing.etag.as_deref()),
        )
        .await?;

    state
        .storage
        .upsert_item_map(
            owner,
            &collection_href,
            &resource_href,
            request_id,
            &item.uid,
            &etag,
        )
        .await?;
    Ok(())
}

pub(crate) fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn class_placeholder_app_data(content_class: &str, owner: &str) -> String {
    match content_class.to_ascii_lowercase().as_str() {
        "contacts" => format!(
            "<Contacts:FirstName>Exchange</Contacts:FirstName><Contacts:LastName>User</Contacts:LastName><Contacts:Email1Address>{}</Contacts:Email1Address><Contacts:CompanyName>Stalwart</Contacts:CompanyName>",
            xml_escape(owner)
        ),
        "tasks" => "<Tasks:Subject>Welcome Task</Tasks:Subject><Tasks:Importance>1</Tasks:Importance><Tasks:Complete>0</Tasks:Complete>".to_string(),
        "notes" => "<Notes:Subject>Welcome Note</Notes:Subject><Notes:MessageClass>IPM.StickyNote</Notes:MessageClass><Notes:Body>Exchange Gateway notes class sync profile.</Notes:Body>".to_string(),
        "documentlibrary" | "documents" => "<DocumentLibrary:DisplayName>Gateway Document</DocumentLibrary:DisplayName><DocumentLibrary:IsFolder>0</DocumentLibrary:IsFolder>".to_string(),
        "sms" | "text" => "<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>SMS profile enabled</AirSyncBase:Data></AirSyncBase:Body>".to_string(),
        "rightsmanagement" => "<RightsManagement:RightsManagementSupport>1</RightsManagement:RightsManagementSupport>".to_string(),
        _ => String::new(),
    }
}

fn parse_datetime(val: &str) -> Option<chrono::DateTime<Utc>> {
    if val.ends_with('Z') {
        chrono::NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%SZ")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
            .or_else(|| {
                chrono::DateTime::parse_from_rfc3339(val)
                    .ok()
                    .map(|dt| dt.with_timezone(&Utc))
            })
    } else if val.contains('T') {
        chrono::NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
            .or_else(|| {
                chrono::NaiveDateTime::parse_from_str(val, "%Y-%m-%dT%H:%M:%S")
                    .map(|dt| Utc.from_utc_datetime(&dt))
                    .ok()
            })
    } else {
        chrono::NaiveDate::parse_from_str(val, "%Y%m%d")
            .ok()
            .and_then(|d| d.and_hms_opt(0, 0, 0))
            .map(|dt| Utc.from_utc_datetime(&dt))
    }
}

fn map_rrule_to_recurrence_xml(rrule: &str) -> Option<String> {
    let parts: Vec<&str> = rrule.split(';').collect();
    let mut freq: Option<u8> = None;
    let mut interval = 1u32;
    let mut day_of_week = String::new();
    let mut day_of_month = 1u32;
    let mut month_of_year = 1u32;
    let mut week_of_month = 0i32;
    let mut until: Option<String> = None;
    let mut occurrences: Option<u32> = None;
    let mut first_day_of_week: Option<u32> = None;

    for part in parts {
        if let Some(idx) = part.find('=') {
            let k = &part[..idx];
            let v = &part[idx + 1..];
            match k {
                "FREQ" => match v {
                    "DAILY" => freq = Some(0),
                    "WEEKLY" => freq = Some(1),
                    "MONTHLY" => freq = Some(2),
                    "YEARLY" => freq = Some(5),
                    _ => {}
                },
                "INTERVAL" => interval = v.parse().unwrap_or(1),
                "BYDAY" => {
                    let mut mask = 0u8;
                    for d in v.split(',') {
                        let (ordinal, day_code) = if d.len() > 2 {
                            (&d[..d.len() - 2], &d[d.len() - 2..])
                        } else {
                            ("", d)
                        };
                        if !ordinal.is_empty() {
                            week_of_month = ordinal.parse::<i32>().unwrap_or(0);
                        }
                        match day_code {
                            "SU" => mask |= 1,
                            "MO" => mask |= 2,
                            "TU" => mask |= 4,
                            "WE" => mask |= 8,
                            "TH" => mask |= 16,
                            "FR" => mask |= 32,
                            "SA" => mask |= 64,
                            _ => {}
                        }
                    }
                    day_of_week = mask.to_string();
                }
                "BYMONTHDAY" => day_of_month = v.parse().unwrap_or(1),
                "BYMONTH" => month_of_year = v.parse().unwrap_or(1),
                "UNTIL" => {
                    if let Some(dt) = parse_datetime(v) {
                        until = Some(dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());
                    }
                }
                "COUNT" => occurrences = v.parse().ok(),
                "WKST" => {
                    first_day_of_week = Some(match v {
                        "SU" => 1,
                        "MO" => 2,
                        "TU" => 3,
                        "WE" => 4,
                        "TH" => 5,
                        "FR" => 6,
                        "SA" => 7,
                        _ => 2,
                    });
                }
                _ => {}
            }
        }
    }

    let mut freq_val = freq?;
    if week_of_month != 0 {
        freq_val = match freq_val {
            2 => 3,
            5 => 6,
            other => other,
        };
    }

    let mut xml = String::from("<Calendar:Recurrence>");
    xml.push_str(&format!("<Calendar:Type>{freq_val}</Calendar:Type>"));
    xml.push_str(&format!(
        "<Calendar:Interval>{interval}</Calendar:Interval>"
    ));
    if !day_of_week.is_empty() {
        xml.push_str(&format!(
            "<Calendar:DayOfWeek>{}</Calendar:DayOfWeek>",
            day_of_week
        ));
    }
    if matches!(freq_val, 2 | 3 | 5 | 6) {
        xml.push_str(&format!(
            "<Calendar:DayOfMonth>{}</Calendar:DayOfMonth>",
            day_of_month
        ));
    }
    if matches!(freq_val, 3 | 6) && week_of_month != 0 {
        let normalized = if week_of_month < 0 {
            5
        } else {
            week_of_month as u32
        };
        xml.push_str(&format!(
            "<Calendar:WeekOfMonth>{}</Calendar:WeekOfMonth>",
            normalized
        ));
    }
    if matches!(freq_val, 5 | 6) {
        xml.push_str(&format!(
            "<Calendar:MonthOfYear>{}</Calendar:MonthOfYear>",
            month_of_year
        ));
    }
    if matches!(freq_val, 2 | 3 | 5 | 6) {
        xml.push_str("<Calendar:CalendarType>0</Calendar:CalendarType>");
    }
    if let Some(v) = until {
        xml.push_str(&format!(
            "<Calendar:Until>{}</Calendar:Until>",
            xml_escape(&v)
        ));
    }
    if let Some(v) = occurrences {
        xml.push_str(&format!(
            "<Calendar:Occurrences>{}</Calendar:Occurrences>",
            v
        ));
    }
    if let Some(v) = first_day_of_week {
        xml.push_str(&format!(
            "<Calendar:FirstDayOfWeek>{}</Calendar:FirstDayOfWeek>",
            v
        ));
    }
    xml.push_str("</Calendar:Recurrence>");
    Some(xml)
}

fn render_attendee_xml(attendee: &Attendee) -> String {
    let mut xml = String::from("<Calendar:Attendee>");
    if !attendee.email.is_empty() {
        xml.push_str(&format!(
            "<Calendar:Email>{}</Calendar:Email>",
            xml_escape(&attendee.email)
        ));
    }
    if let Some(name) = attendee.name.as_deref().filter(|v| !v.is_empty()) {
        xml.push_str(&format!(
            "<Calendar:Name>{}</Calendar:Name>",
            xml_escape(name)
        ));
    } else if !attendee.email.is_empty() {
        xml.push_str(&format!(
            "<Calendar:Name>{}</Calendar:Name>",
            xml_escape(&attendee.email)
        ));
    }
    if let Some(v) = attendee.attendee_type {
        xml.push_str(&format!(
            "<Calendar:AttendeeType>{}</Calendar:AttendeeType>",
            v
        ));
    }
    if let Some(v) = attendee.attendee_status {
        xml.push_str(&format!(
            "<Calendar:AttendeeStatus>{}</Calendar:AttendeeStatus>",
            v
        ));
    }
    xml.push_str("</Calendar:Attendee>");
    xml
}

fn derived_meeting_status(item: &CalendarItem) -> u8 {
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

fn derived_response_type(item: &CalendarItem) -> Option<u8> {
    if let Some(v) = item.response_type {
        return Some(v);
    }
    if derived_meeting_status(item) == 1 {
        return Some(1);
    }
    item.attendees.iter().find_map(|attendee| match attendee.attendee_status {
        Some(2) => Some(2),
        Some(3) => Some(3),
        Some(4) => Some(4),
        Some(5) => Some(5),
        _ => None,
    })
}

fn render_exception_xml(exception: &CalendarException, item: &CalendarItem) -> String {
    let mut xml = String::from("<Calendar:Exception>");
    xml.push_str(&format!(
        "<Calendar:ExceptionStartTime>{}</Calendar:ExceptionStartTime>",
        exception.exception_start.format("%Y-%m-%dT%H:%M:%SZ")
    ));

    if exception.deleted {
        xml.push_str("<Calendar:Deleted>1</Calendar:Deleted>");
        xml.push_str("</Calendar:Exception>");
        return xml;
    }

    if let Some(v) = &exception.subject {
        xml.push_str(&format!(
            "<Calendar:Subject>{}</Calendar:Subject>",
            xml_escape(v)
        ));
    }
    if let Some(v) = exception.start {
        xml.push_str(&format!(
            "<Calendar:StartTime>{}</Calendar:StartTime>",
            v.format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    if let Some(v) = exception.end {
        xml.push_str(&format!(
            "<Calendar:EndTime>{}</Calendar:EndTime>",
            v.format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    if let Some(v) = exception.all_day {
        xml.push_str(if v {
            "<Calendar:AllDayEvent>1</Calendar:AllDayEvent>"
        } else {
            "<Calendar:AllDayEvent>0</Calendar:AllDayEvent>"
        });
    }
    if let Some(v) = &exception.location {
        xml.push_str(&format!(
            "<Calendar:Location>{}</Calendar:Location>",
            xml_escape(v)
        ));
    }
    if let Some(v) = exception.busy_status {
        xml.push_str(&format!("<Calendar:BusyStatus>{}</Calendar:BusyStatus>", v));
    }
    if let Some(v) = exception.sensitivity {
        xml.push_str(&format!(
            "<Calendar:Sensitivity>{}</Calendar:Sensitivity>",
            v
        ));
    }
    if let Some(v) = exception.reminder {
        xml.push_str(&format!("<Calendar:Reminder>{}</Calendar:Reminder>", v));
    }
    if let Some(v) = exception.appointment_reply_time {
        xml.push_str(&format!(
            "<Calendar:AppointmentReplyTime>{}</Calendar:AppointmentReplyTime>",
            v.format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    if let Some(v) = exception.meeting_status {
        xml.push_str(&format!(
            "<Calendar:MeetingStatus>{}</Calendar:MeetingStatus>",
            v
        ));
    } else {
        xml.push_str(&format!(
            "<Calendar:MeetingStatus>{}</Calendar:MeetingStatus>",
            derived_meeting_status(item)
        ));
    }
    if let Some(v) = exception.response_type.or_else(|| derived_response_type(item)) {
        xml.push_str(&format!(
            "<Calendar:ResponseType>{}</Calendar:ResponseType>",
            v
        ));
    }

    if let Some(v) = &exception.categories {
        if !v.is_empty() {
            xml.push_str("<Calendar:Categories>");
            for category in v {
                xml.push_str(&format!(
                    "<Calendar:Category>{}</Calendar:Category>",
                    xml_escape(category)
                ));
            }
            xml.push_str("</Calendar:Categories>");
        }
    } else if !item.categories.is_empty() {
        xml.push_str("<Calendar:Categories>");
        for category in &item.categories {
            xml.push_str(&format!(
                "<Calendar:Category>{}</Calendar:Category>",
                xml_escape(category)
            ));
        }
        xml.push_str("</Calendar:Categories>");
    }

    if let Some(attendees) = &exception.attendees
        && !attendees.is_empty()
    {
        xml.push_str("<Calendar:Attendees>");
        for attendee in attendees {
            xml.push_str(&render_attendee_xml(attendee));
        }
        xml.push_str("</Calendar:Attendees>");
    }

    if let Some(v) = &exception.description {
        xml.push_str("<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type>");
        xml.push_str(&format!(
            "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
            v.len()
        ));
        xml.push_str("<AirSyncBase:Truncated>0</AirSyncBase:Truncated><AirSyncBase:Data>");
        xml.push_str(&xml_escape(v));
        xml.push_str("</AirSyncBase:Data></AirSyncBase:Body>");
    }

    xml.push_str("</Calendar:Exception>");
    xml
}

pub(crate) fn render_calendar_app_data(item: &CalendarItem) -> String {
    let mut xml = String::new();

    xml.push_str(&format!(
        "<Calendar:Subject>{}</Calendar:Subject>",
        xml_escape(&item.subject)
    ));
    xml.push_str(&format!(
        "<Calendar:StartTime>{}</Calendar:StartTime>",
        item.start.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    xml.push_str(&format!(
        "<Calendar:EndTime>{}</Calendar:EndTime>",
        item.end.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    xml.push_str(&format!(
        "<Calendar:DtStamp>{}</Calendar:DtStamp>",
        item.dtstamp
            .unwrap_or_else(Utc::now)
            .format("%Y-%m-%dT%H:%M:%SZ")
    ));
    xml.push_str(if item.all_day {
        "<Calendar:AllDayEvent>1</Calendar:AllDayEvent>"
    } else {
        "<Calendar:AllDayEvent>0</Calendar:AllDayEvent>"
    });

    if !item.all_day
        && let Some(v) = &item.timezone
    {
        xml.push_str(&format!(
            "<Calendar:Timezone>{}</Calendar:Timezone>",
            xml_escape(v)
        ));
    }

    if let Some(v) = item.busy_status {
        xml.push_str(&format!("<Calendar:BusyStatus>{}</Calendar:BusyStatus>", v));
    }
    if let Some(v) = item.sensitivity {
        xml.push_str(&format!(
            "<Calendar:Sensitivity>{}</Calendar:Sensitivity>",
            v
        ));
    }
    if let Some(v) = item.reminder {
        xml.push_str(&format!("<Calendar:Reminder>{}</Calendar:Reminder>", v));
    }
    if !item.location.is_empty() {
        xml.push_str(&format!(
            "<Calendar:Location>{}</Calendar:Location>",
            xml_escape(&item.location)
        ));
    }
    if let Some(v) = &item.organizer_name {
        xml.push_str(&format!(
            "<Calendar:OrganizerName>{}</Calendar:OrganizerName>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.organizer_email {
        xml.push_str(&format!(
            "<Calendar:OrganizerEmail>{}</Calendar:OrganizerEmail>",
            xml_escape(v)
        ));
    }

    if !item.attendees.is_empty() {
        xml.push_str("<Calendar:Attendees>");
        for attendee in &item.attendees {
            xml.push_str(&render_attendee_xml(attendee));
        }
        xml.push_str("</Calendar:Attendees>");
    }

    if !item.categories.is_empty() {
        xml.push_str("<Calendar:Categories>");
        for category in &item.categories {
            xml.push_str(&format!(
                "<Calendar:Category>{}</Calendar:Category>",
                xml_escape(category)
            ));
        }
        xml.push_str("</Calendar:Categories>");
    }

    xml.push_str("<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type>");
    xml.push_str(&format!(
        "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
        item.description.len()
    ));
    xml.push_str("<AirSyncBase:Truncated>0</AirSyncBase:Truncated><AirSyncBase:Data>");
    xml.push_str(&xml_escape(&item.description));
    xml.push_str("</AirSyncBase:Data></AirSyncBase:Body>");
    xml.push_str(&format!(
        "<Calendar:UID>{}</Calendar:UID>",
        xml_escape(&item.uid)
    ));

    if let Some(rrule) = &item.rrule
        && let Some(rec_xml) = map_rrule_to_recurrence_xml(rrule)
    {
        xml.push_str(&rec_xml);
    }

    if !item.exceptions.is_empty() {
        xml.push_str("<Calendar:Exceptions>");
        for exception in &item.exceptions {
            xml.push_str(&render_exception_xml(exception, item));
        }
        xml.push_str("</Calendar:Exceptions>");
    }

    xml.push_str(&format!(
        "<Calendar:MeetingStatus>{}</Calendar:MeetingStatus>",
        derived_meeting_status(item)
    ));

    if let Some(v) = item.response_requested {
        xml.push_str(&format!(
            "<Calendar:ResponseRequested>{}</Calendar:ResponseRequested>",
            if v { 1 } else { 0 }
        ));
    }
    if let Some(v) = item.disallow_new_time_proposal {
        xml.push_str(&format!(
            "<Calendar:DisallowNewTimeProposal>{}</Calendar:DisallowNewTimeProposal>",
            if v { 1 } else { 0 }
        ));
    }
    if let Some(v) = item.appointment_reply_time {
        xml.push_str(&format!(
            "<Calendar:AppointmentReplyTime>{}</Calendar:AppointmentReplyTime>",
            v.format("%Y-%m-%dT%H:%M:%SZ")
        ));
    }
    if let Some(v) = derived_response_type(item) {
        xml.push_str(&format!(
            "<Calendar:ResponseType>{}</Calendar:ResponseType>",
            v
        ));
    }
    if let Some(v) = &item.online_meeting_conf_link {
        xml.push_str(&format!(
            "<Calendar:OnlineMeetingConfLink>{}</Calendar:OnlineMeetingConfLink>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.online_meeting_external_link {
        xml.push_str(&format!(
            "<Calendar:OnlineMeetingExternalLink>{}</Calendar:OnlineMeetingExternalLink>",
            xml_escape(v)
        ));
    }
    if let Some(v) = &item.client_uid {
        xml.push_str(&format!(
            "<Calendar:ClientUid>{}</Calendar:ClientUid>",
            xml_escape(v)
        ));
    }

    xml
}

pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    state_collection_id: &str,
    incoming_sync_key: &str,
    content_class: &str,
    _window_size: usize,
    username: &str,
    password: &str,
    client_mutation_responses: &str,
) -> Result<String> {
    let storage = &state.storage;

    if !content_class.eq_ignore_ascii_case("Calendar") {
        let class_name = content_class.trim();
        let normalized = if class_name.is_empty() {
            "Calendar"
        } else {
            class_name
        };

        let new_sync_key = Uuid::new_v4().to_string();
        storage
            .set_sync_key(owner, state_collection_id, &new_sync_key, Some("token"))
            .await?;

        let pseudo_resource = format!("class://{}/{}", owner, normalized.to_ascii_lowercase());
        let server_id = generate_server_id(&state.cfg.hmac_secret, &pseudo_resource);
        let app_data = class_placeholder_app_data(normalized, owner);
        let commands = if app_data.is_empty() {
            String::new()
        } else {
            format!(
                "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
                xml_escape(&server_id),
                app_data
            )
        };

        return Ok(format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Contacts="Contacts:" xmlns:Tasks="Tasks:" xmlns:Notes="Notes:" xmlns:DocumentLibrary="DocumentLibrary:" xmlns:RightsManagement="RightsManagement:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>{}</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>1</Status>
{}<Commands>{}</Commands>
</Collection></Collections>
</Sync>"#,
            xml_escape(normalized),
            new_sync_key,
            xml_escape(collection_id),
            client_mutation_responses,
            commands
        ));
    }

    let previous_state = storage.get_sync_key(owner, state_collection_id).await?;
    if incoming_sync_key != "0" {
        match previous_state.as_ref() {
            Some((expected_sync_key, _)) if expected_sync_key == incoming_sync_key => {}
            _ => return Ok(invalid_sync_key_response(collection_id, "Calendar")),
        }
    }

    let since = if incoming_sync_key == "0" {
        0
    } else {
        sync_since_from_token(
            previous_state
                .as_ref()
                .and_then(|(_, token)| token.as_deref()),
        )
    };

    let latest_seq = storage.get_latest_change_seq().await.unwrap_or(0);
    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow!("no calendars found"))?
        .clone();

    let start = (Utc::now() - chrono::Duration::weeks(52))
        .format("%Y%m%dT%H%M%SZ")
        .to_string();
    let end = (Utc::now() + chrono::Duration::weeks(52))
        .format("%Y%m%dT%H%M%SZ")
        .to_string();

    let events_xml = caldav
        .query_events(&collection_href, &start, &end, username, password)
        .await?;

    use quick_xml::events::Event;
    use quick_xml::Reader;

    #[derive(Clone)]
    struct EventItem {
        href: String,
        etag: String,
        ics: String,
    }

    let mut reader = Reader::from_str(&events_xml);
    reader.config_mut().trim_text(true);

    let mut events = Vec::new();
    let mut current = EventItem {
        href: String::new(),
        etag: String::new(),
        ics: String::new(),
    };
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                b"href" => {
                    if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                        current.href = e.decode().unwrap_or_default().to_string();
                    }
                }
                b"getetag" => {
                    if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                        current.etag = e.decode().unwrap_or_default().to_string();
                    }
                }
                b"calendar-data" => {
                    if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                        current.ics = e.decode().unwrap_or_default().to_string();
                    }
                }
                _ => {}
            },
            Ok(Event::End(ref e)) if e.name().local_name().as_ref() == b"response" => {
                if !current.href.is_empty() {
                    events.push(current.clone());
                }
                current = EventItem {
                    href: String::new(),
                    etag: String::new(),
                    ics: String::new(),
                };
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }

    let existing_map = storage
        .list_ews_items(owner, 4096, 0)
        .await?
        .into_iter()
        .map(|item| (item.server_id.clone(), item))
        .collect::<HashMap<_, _>>();

    let mut commands = String::new();
    let mut seen_ids = HashSet::new();
    let initial_sync = incoming_sync_key == "0";

    for ev in events {
        if ev.href.is_empty() {
            continue;
        }

        let resource_href = ev.href.clone();
        let server_id = generate_server_id(&state.cfg.hmac_secret, &resource_href);
        seen_ids.insert(server_id.clone());

        let etag = ev.etag.trim_matches('"').to_string();
        let Some(item) = parse_ics_event(&ev.ics) else {
            continue;
        };

        let existing = existing_map.get(&server_id);
        let is_new = existing.is_none();
        let changed = existing
            .map(|row| row.etag.as_deref() != Some(etag.as_str()))
            .unwrap_or(true);

        if changed {
            storage
                .upsert_item_map(
                    owner,
                    &collection_href,
                    &resource_href,
                    &server_id,
                    &item.uid,
                    &etag,
                )
                .await?;
        }

        if !initial_sync && !changed {
            continue;
        }

        if is_new {
            commands.push_str("<Add><ServerId>");
        } else {
            commands.push_str("<Change><ServerId>");
        }
        commands.push_str(&server_id);
        commands.push_str("</ServerId><ApplicationData>");
        commands.push_str(&render_calendar_app_data(&item));
        if is_new {
            commands.push_str("</ApplicationData></Add>");
        } else {
            commands.push_str("</ApplicationData></Change>");
        }
    }

    for server_id in existing_map.keys() {
        if !seen_ids.contains(server_id) {
            let _ = storage.add_delete_tombstone(owner, server_id).await;
            let _ = storage.delete_item_by_server_id(owner, server_id).await;
        }
    }

    let deleted_ids = if initial_sync {
        Vec::new()
    } else {
        storage
            .list_deleted_since_seq(owner, since)
            .await?
            .into_iter()
            .map(|(_, server_id)| server_id)
            .collect()
    };

    for deleted_id in deleted_ids {
        if !seen_ids.contains(&deleted_id) {
            commands.push_str(&format!(
                "<Delete><ServerId>{}</ServerId></Delete>",
                xml_escape(&deleted_id)
            ));
        }
    }

    let new_sync_key = Uuid::new_v4().to_string();
    storage
        .set_sync_key(
            owner,
            state_collection_id,
            &new_sync_key,
            Some(&sync_seq_to_token(latest_seq)),
        )
        .await?;

    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>Calendar</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>1</Status>
{}<Commands>{}</Commands>
</Collection></Collections>
</Sync>"#,
        new_sync_key, collection_id, client_mutation_responses, commands
    ))
}
