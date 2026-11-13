// src/sync.rs
use crate::caldav::CaldavClient;
use crate::calendar::{
    Attendee, CalendarException, CalendarItem, EasSyncMutation, parse_datetime,
    parse_eas_sync_mutations, parse_ics_event, render_ics,
};
use crate::models::AppState;
use crate::storage::CalendarDeliveryWrite;
use crate::util::{resolve_xml_reference, xml_escape};
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use secrecy::SecretString;
use sha2::Sha256;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// [MS-ASCMD] §2.2.3.177.17: Sync collection status 3 = "Invalid
/// synchronization key". (FolderSync uses 9; GetItemEstimate uses 4.)
pub const INVALID_SYNC_KEY_STATUS: &str = "3";
const DEFAULT_WINDOW_SIZE: usize = 100;
const MAX_WINDOW_SIZE: usize = 512;

pub struct PerformSyncParams<'a> {
    pub state: Arc<AppState>,
    pub owner: &'a str,
    pub collection_id: &'a str,
    pub state_collection_id: &'a str,
    pub incoming_sync_key: &'a str,
    pub content_class: &'a str,
    pub opts: SyncOptions,
    pub username: &'a str,
    pub password: &'a str,
    pub client_mutation_responses: &'a str,
}

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
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 key must be non-empty: check hmac_secret configuration");
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

pub fn filter_type_to_start(filter_type: u8) -> chrono::DateTime<Utc> {
    let now = Utc::now();
    match filter_type {
        0 => now - Duration::weeks(520),
        1 => now - Duration::days(1),
        2 => now - Duration::days(3),
        3 => now - Duration::weeks(1),
        4 => now - Duration::weeks(2),
        5 => now - Duration::days(30),
        6 => now - Duration::days(90),
        7 => now - Duration::days(180),
        _ => now - Duration::weeks(52),
    }
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

/// Resolve the CalDAV collection href backing a user-created calendar-family
/// EAS folder from its `CollectionId`, or `None` for a system folder (which
/// keeps the default-calendar behavior).
///
/// A `folder_map` READ failure is propagated as an error, never flattened to
/// `None`: `None` means "system or genuinely unmapped", and the callers treat
/// it as an unscoped collection (the default calendar, or account-wide JMAP
/// events). Conflating a transient storage error with that would let a custom
/// folder Sync leak the default calendar's events; the caller must instead fail
/// retryably.
async fn collection_folder_href(
    state: &Arc<AppState>,
    owner: &str,
    collection_id: &str,
) -> Result<Option<String>> {
    // Callers may pass either the visible CollectionId (the calendar Sync
    // path) or its device-scoped form `{visible}::{device}` (the client-mutation
    // path); `folder_map` is keyed by the visible id. A CalDAV collection href
    // is a valid CollectionId that may itself contain `::`, so the COMPLETE id
    // is looked up first and the device suffix is stripped only when that
    // fails — otherwise `Foo::Bar/` would resolve to `Foo`.
    if let Some(href) = folder_caldav_href(state, owner, collection_id).await? {
        return Ok(Some(href));
    }
    match collection_id.rsplit_once("::") {
        Some((visible, _)) => folder_caldav_href(state, owner, visible).await,
        None => Ok(None),
    }
}

/// The CalDAV collection href a `folder_map` row backs, or `None` for a system
/// folder or an unmapped id. A storage read failure is an error (see
/// [`collection_folder_href`]).
async fn folder_caldav_href(
    state: &Arc<AppState>,
    owner: &str,
    server_id: &str,
) -> Result<Option<String>> {
    if crate::eas_folders::is_system_folder_id(server_id) {
        return Ok(None);
    }
    match state.storage.get_folder(owner, server_id).await {
        Ok(Some(row)) if row.backend_kind == "caldav" => Ok(Some(row.backend_id)),
        Ok(_) => Ok(None),
        Err(e) => Err(anyhow!(
            "failed to resolve calendar folder {server_id}: {e}"
        )),
    }
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

    let caldav = CaldavClient::new(&state.cfg)?;
    // Scope mutations to the folder the client is syncing: a custom
    // calendar-family folder ([MS-ASCMD] §2.2.3.186.2 Types 13/15/16/17) has
    // its CalDAV collection href as its CollectionId, so an Add there must land
    // in that collection, not the first calendar returned. A system Calendar
    // folder (CollectionId "1") keeps the default-calendar behavior.
    let collection_href = match collection_folder_href(&state, owner, collection_id).await? {
        Some(href) => caldav.absolute_url(&href)?,
        None => {
            let calendars = caldav.find_user_calendars(username, password).await?;
            calendars
                .first()
                .ok_or_else(|| anyhow!("no calendars found"))?
                .clone()
        }
    };
    let mut results = Vec::new();

    for mutation in mutations {
        match mutation {
            EasSyncMutation::Add {
                client_id,
                item,
                attachment_adds,
            } => {
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
                // A1: default ORGANIZER to the user's primary SMTP for meetings.
                let owner_email = crate::util::user_primary_email(username, &state.cfg.mail_domain);
                crate::calendar::ensure_organizer_for_scheduling(&mut item, owner_email.as_deref());
                let ics = render_ics(&item);
                match caldav
                    .put_event(&collection_href, None, &ics, username, password, None)
                    .await
                {
                    Ok((resource_href, etag)) => {
                        let server_id = generate_server_id(state.cfg.hmac_secret(), &resource_href);
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
                        // The client already holds this item (it just created it),
                        // so record it in this scope's ledger: the follow-up Sync
                        // must filter it out instead of re-emitting `<Add>`.
                        state
                            .storage
                            .upsert_calendar_delivery(owner, collection_id, &server_id, &etag)
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
                        for att in &attachment_adds {
                            let name = if att.display_name.is_empty() {
                                "attachment.dat"
                            } else {
                                &att.display_name
                            };
                            let content_type = if att.content_type.is_empty() {
                                crate::attachment::mime_type_for_filename(name).to_string()
                            } else {
                                att.content_type.clone()
                            };
                            if let Ok(created) = state
                                .attachment_manager
                                .create_file_attachment(
                                    &crate::attachment::CreateAttachmentParams {
                                        owner,
                                        parent_item_server_id: &server_id,
                                        name,
                                        content_type: &content_type,
                                        content_base64: &att.content_base64,
                                        is_inline: att.is_inline,
                                        content_id: att.content_id.as_deref(),
                                        content_location: att.content_location.as_deref(),
                                    },
                                )
                                .await
                            {
                                tracing::debug!(
                                    attachment_id = %created.id,
                                    parent = %server_id,
                                    "Created attachment from EAS sync Add"
                                );
                            }
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
            EasSyncMutation::Change {
                server_id,
                patch,
                attachment_adds,
                attachment_deletes,
                ..
            } => {
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
                // A1: default ORGANIZER to the user's primary SMTP for meetings.
                let owner_email = crate::util::user_primary_email(username, &state.cfg.mail_domain);
                crate::calendar::ensure_organizer_for_scheduling(&mut item, owner_email.as_deref());
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
                        // The client just applied this change locally; record the
                        // new ETag so the follow-up Sync does not re-emit
                        // `<Change>` for it.
                        state
                            .storage
                            .upsert_calendar_delivery(owner, collection_id, &server_id, &etag)
                            .await?;
                        for att_id in &attachment_deletes {
                            if let Err(e) = state
                                .attachment_manager
                                .delete_attachment(owner, att_id)
                                .await
                            {
                                tracing::warn!(
                                    attachment_id = %att_id,
                                    error = %e,
                                    "Failed to delete attachment during EAS sync Change"
                                );
                            }
                        }
                        for att in &attachment_adds {
                            let name = if att.display_name.is_empty() {
                                "attachment.dat"
                            } else {
                                &att.display_name
                            };
                            let content_type = if att.content_type.is_empty() {
                                crate::attachment::mime_type_for_filename(name).to_string()
                            } else {
                                att.content_type.clone()
                            };
                            if let Ok(created) = state
                                .attachment_manager
                                .create_file_attachment(
                                    &crate::attachment::CreateAttachmentParams {
                                        owner,
                                        parent_item_server_id: &server_id,
                                        name,
                                        content_type: &content_type,
                                        content_base64: &att.content_base64,
                                        is_inline: att.is_inline,
                                        content_id: att.content_id.as_deref(),
                                        content_location: att.content_location.as_deref(),
                                    },
                                )
                                .await
                            {
                                tracing::debug!(
                                    attachment_id = %created.id,
                                    parent = %server_id,
                                    "Created attachment from EAS sync Change"
                                );
                            }
                        }
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
            EasSyncMutation::Delete { server_id, .. } => {
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
                        state
                            .storage
                            .add_delete_tombstone(owner, &server_id)
                            .await?;
                        state
                            .storage
                            .delete_item_by_server_id(owner, &server_id)
                            .await?;
                        // The client applied this delete locally; drop this
                        // scope's ledger row so a later re-add is an `<Add>`.
                        state
                            .storage
                            .delete_calendar_delivery(owner, collection_id, &server_id)
                            .await?;
                        if let Err(e) = state
                            .attachment_manager
                            .delete_attachments_for_item(owner, &server_id)
                            .await
                        {
                            tracing::warn!(
                                server_id = %server_id,
                                error = %e,
                                "Failed to clean up attachments during EAS sync Delete"
                            );
                        }
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
    let mut xml = String::with_capacity(512);
    xml.push_str("<Responses>");
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

pub(crate) fn map_rrule_to_recurrence_xml(rrule: &str) -> Option<String> {
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
                    })
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
    let mut xml = String::with_capacity(512);
    xml.push_str("<Calendar:Recurrence>");
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
    // NOTE: <Calendar:StartTimeZone>/<EndTimeZone> are item-level calendar
    // properties (rendered standalone from the item's zone in
    // `render_calendar_app_data`), NOT legal children of <Calendar:Recurrence>
    // per MS-ASCAL §2.2.3.8. The recurrence time-zone is conveyed by the
    // parent <Calendar:Timezone> blob; emitting StartTimeZone/EndTimeZone here
    // (as the legacy code did, with a raw IANA string) was both malformed and
    // drifted from the authoritative iCalendar RRULE boundaries, so it is no
    // longer produced inside the Recurrence body.
    xml.push_str("</Calendar:Recurrence>");
    Some(xml)
}

fn render_attendee_xml(attendee: &Attendee) -> String {
    let mut xml = String::with_capacity(256);
    xml.push_str("<Calendar:Attendee>");
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
    item.attendees.iter().find_map(|a| match a.attendee_status {
        Some(2) => Some(2),
        Some(3) => Some(3),
        Some(4) => Some(4),
        Some(5) => Some(5),
        _ => None,
    })
}

fn render_exception_xml(
    exception: &CalendarException,
    item: &CalendarItem,
    options: &crate::eas_sync_options::EasSyncCollectionOptions,
) -> String {
    let mut xml = String::with_capacity(1024);
    xml.push_str("<Calendar:Exception>");
    xml.push_str(&format!(
        "<Calendar:ExceptionStartTime>{}</Calendar:ExceptionStartTime>",
        exception.exception_start.format("%Y-%m-%dT%H:%M:%SZ")
    ));
    if exception.deleted {
        xml.push_str("<Calendar:Deleted>1</Calendar:Deleted></Calendar:Exception>");
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
        xml.push_str(&format!("<AirSyncBase:Location><AirSyncBase:DisplayName>{}</AirSyncBase:DisplayName></AirSyncBase:Location>", xml_escape(v)));
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
    if let Some(v) = exception
        .response_type
        .or_else(|| derived_response_type(item))
    {
        xml.push_str(&format!(
            "<Calendar:ResponseType>{}</Calendar:ResponseType>",
            v
        ));
    }
    if let Some(cats) = &exception.categories
        && !cats.is_empty()
    {
        xml.push_str("<Calendar:Categories>");
        for cat in cats {
            xml.push_str(&format!(
                "<Calendar:Category>{}</Calendar:Category>",
                xml_escape(cat)
            ));
        }
        xml.push_str("</Calendar:Categories>");
    } else if !item.categories.is_empty() {
        xml.push_str("<Calendar:Categories>");
        for cat in &item.categories {
            xml.push_str(&format!(
                "<Calendar:Category>{}</Calendar:Category>",
                xml_escape(cat)
            ));
        }
        xml.push_str("</Calendar:Categories>");
    }
    if let Some(attendees) = &exception.attendees
        && !attendees.is_empty()
    {
        xml.push_str("<Calendar:Attendees>");
        for a in attendees {
            xml.push_str(&render_attendee_xml(a));
        }
        xml.push_str("</Calendar:Attendees>");
    }
    if let Some(desc) = &exception.description {
        // The negotiated options apply to exception bodies exactly as to the
        // parent item's body ([MS-ASCMD] §2.2.3.125.6 options are
        // collection-scoped, not item-scoped).
        xml.push_str(&render_calendar_body_xml(desc, options));
    }
    xml.push_str("</Calendar:Exception>");
    xml
}

/// True when `tz` is a UTC (zero-offset, no-DST) id, i.e. an event whose
/// wall-clock equals its UTC instant and needs no EAS `Timezone` blob. EAS
/// clients treat a UTC event's times as absolute instants; emitting a TZI
/// blob for UTC is redundant and several clients render it as an
/// unspecified-failure when the blob's transitions are all zeroed, so the
/// canonical UTC ids are omitted (MS-ASCAL §2.2.3.9: the `Timezone` element
/// is "OPTIONAL" and only present for non-UTC events).
fn is_utc_zone(tz: &str) -> bool {
    matches!(
        tz,
        "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Etc/GMT0" | "Etc/GMT+0" | "Etc/GMT-0"
    )
}

/// Render the `<AirSyncBase:Body>` element for a calendar item, honoring the
/// negotiated BodyPreference chain ([MS-ASAIRS] §2.2.2.12, §2.2.2.3.2).
///
/// The native storage format of a calendar description is plain text
/// ([MS-ASAIRS] §2.2.2.41.1). For protocol versions 16.0 and 16.1 the only
/// valid Type values for calendar items are 1 (plain text) and 2 (HTML), so
/// RTF/MIME preferences fall back to plain text.
fn render_calendar_body_xml(
    description: &str,
    options: &crate::eas_sync_options::EasSyncCollectionOptions,
) -> String {
    use crate::eas_sync_options::{NativeBodyType, negotiate_body, truncate_utf8_bytes};

    let negotiated = negotiate_body(options, NativeBodyType::PlainText, description.len());
    let mut body_type = match negotiated.body_type {
        1 | 2 => negotiated.body_type,
        _ => 1,
    };
    let mut full_content = if body_type == 2 {
        crate::email::plain_to_minimal_html(description)
    } else {
        description.to_string()
    };
    // Conversion can grow the content (the minimal-HTML wrapper):
    // AllOrNone compares against the bytes actually returned, so
    // re-negotiate with the converted size — and honor the type the
    // re-negotiation selects. A Type 2 chain that fit the plain
    // description can be AllOrNone-skipped once converted; the
    // re-negotiation then falls back exactly like the email renderer,
    // and the served format must follow the selected preference so the
    // emitted <Type>, the truncation limit, and the content all come
    // from the same negotiated entry.
    let mut negotiated = negotiated;
    if body_type == 2 {
        negotiated = negotiate_body(options, NativeBodyType::PlainText, full_content.len());
        let renegotiated_type = match negotiated.body_type {
            1 | 2 => negotiated.body_type,
            _ => body_type,
        };
        if renegotiated_type != body_type {
            body_type = renegotiated_type;
            full_content = if body_type == 2 {
                crate::email::plain_to_minimal_html(description)
            } else {
                description.to_string()
            };
        }
    }
    let true_size = full_content.len();
    let (data, truncated) = if negotiated.data_withheld {
        (String::new(), false)
    } else {
        match negotiated.truncation_size {
            Some(limit) => {
                let (slice, cut) = truncate_utf8_bytes(&full_content, limit);
                (slice.to_owned(), cut)
            }
            None => (full_content.clone(), false),
        }
    };

    let mut xml = String::with_capacity(256);
    xml.push_str(&format!(
        "<AirSyncBase:Body><AirSyncBase:Type>{}</AirSyncBase:Type>",
        body_type
    ));
    xml.push_str(&format!(
        "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
        true_size
    ));
    if truncated {
        xml.push_str("<AirSyncBase:Truncated>1</AirSyncBase:Truncated>");
    }
    if !data.is_empty() {
        // Data is XML character data ([MS-ASDTYPE] §2.3): strip the
        // characters XML 1.0 forbids even in escaped form before escaping
        // the remainder. A withheld body (data_withheld) emits no Data.
        xml.push_str(&format!(
            "<AirSyncBase:Data>{}</AirSyncBase:Data>",
            xml_escape(&crate::email::sanitize_for_xml(&data))
        ));
    }
    // [MS-ASAIRS] §2.2.2.35.1: no Preview alongside a Type 1 body carrying
    // valid Data — the body itself is the preview source. The preview is
    // always cut from the FULL body ([MS-ASAIRS] §2.2.2.35.1: the element
    // "specifies a preview of the message body"), not from the truncated
    // Data slice, so the client's `Preview` length setting keeps its meaning
    // even when TruncationSize cut the body to less.
    if let Some(chars) = negotiated.preview
        && !(body_type == 1 && !data.is_empty())
    {
        let preview_source = if body_type == 2 {
            crate::eas_sync_options::html_to_plain_text(&full_content)
        } else {
            full_content.clone()
        };
        xml.push_str(&format!(
            "<AirSyncBase:Preview>{}</AirSyncBase:Preview>",
            xml_escape(&crate::eas_sync_options::preview_text(
                &crate::email::sanitize_for_xml(&preview_source),
                chars
            ))
        ));
    }
    xml.push_str("</AirSyncBase:Body>");
    xml
}

/// Options-aware calendar AppData: the `<AirSyncBase:Body>` block follows the
/// negotiated BodyPreference chain ([MS-ASAIRS] §2.2.2.12); with no
/// preferences the native plain-text rendition is served untruncated.
pub(crate) fn render_calendar_app_data_with_options(
    item: &CalendarItem,
    options: &crate::eas_sync_options::EasSyncCollectionOptions,
) -> String {
    let mut xml = String::with_capacity(2048);
    xml.push_str(&render_calendar_app_data_prefix(item));
    xml.push_str(&render_calendar_body_xml(&item.description, options));
    xml.push_str("<AirSyncBase:NativeBodyType>1</AirSyncBase:NativeBodyType>");
    xml.push_str(&render_calendar_app_data_suffix(item, options));
    xml
}

/// Everything `<ApplicationData>` emits before the `<AirSyncBase:Body>`
/// block for a calendar item.
fn render_calendar_app_data_prefix(item: &CalendarItem) -> String {
    let mut xml = String::with_capacity(1024);
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
        && !is_utc_zone(v)
    {
        // EAS `Calendar:Timezone` is a base64 Windows TZI blob (MS-ASSETTINGS
        // / MS-ASCAL §2.2.3.9). Convert from the stored IANA id; if unmappable
        // omit the element (Android then interprets times as UTC, matching the
        // prior behaviour). UTC events carry no offset/DST so the blob is
        // omitted for them too (`is_utc_zone`), keeping the element OPTIONAL
        // per spec and avoiding a zeroed-TZI that some clients reject.
        if let Some(blob) = crate::timezone::iana_to_eas_timezone_blob(v) {
            xml.push_str(&format!(
                "<Calendar:Timezone>{}</Calendar:Timezone>",
                xml_escape(&blob)
            ));
        }
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
        xml.push_str(&format!("<AirSyncBase:Location><AirSyncBase:DisplayName>{}</AirSyncBase:DisplayName></AirSyncBase:Location>", xml_escape(&item.location)));
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
        for a in &item.attendees {
            xml.push_str(&render_attendee_xml(a));
        }
        xml.push_str("</Calendar:Attendees>");
    }
    if !item.categories.is_empty() {
        xml.push_str("<Calendar:Categories>");
        for c in &item.categories {
            xml.push_str(&format!(
                "<Calendar:Category>{}</Calendar:Category>",
                xml_escape(c)
            ));
        }
        xml.push_str("</Calendar:Categories>");
    }
    xml
}

/// Everything `<ApplicationData>` emits after the `<AirSyncBase:Body>` and
/// `<AirSyncBase:NativeBodyType>` blocks for a calendar item.
fn render_calendar_app_data_suffix(
    item: &CalendarItem,
    options: &crate::eas_sync_options::EasSyncCollectionOptions,
) -> String {
    let mut xml = String::with_capacity(1024);
    // NOTE: <Calendar:StartTimeZone>/<EndTimeZone> are deliberately NOT
    // emitted. [MS-ASWBXML] code page 4 (Calendar) has NO token slots for
    // them — its only time-zone representation is the <Calendar:Timezone>
    // byte-array blob (§2.2.3.9), which `render_calendar_app_data_prefix`
    // already emits. In a WBXML response these elements would have to be
    // written as literal inline text, which the target clients do not parse
    // as structured calendar data ([MS-ASCAL] §2.2.3.12/§2.2.3.13 define the
    // elements for protocols that carry them, but nothing in the 16.0/16.1
    // client wire shape consumes them here).
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
        for ex in &item.exceptions {
            xml.push_str(&render_exception_xml(ex, item, options));
        }
        xml.push_str("</Calendar:Exceptions>");
    }
    xml.push_str("<Calendar:FirstDayOfWeek>1</Calendar:FirstDayOfWeek>");
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

/// Legacy calendar AppData: no negotiated options, so the native plain-text
/// body is served untruncated ([MS-ASAIRS] §2.2.2.3.2 pass 3). Test-only: the
/// production paths always render with negotiated options.
/// Deleted per AUDIT2.md §30 — no non-test caller remains; tests call
/// `render_calendar_app_data_with_options` directly with default options.
pub struct SyncOptions {
    pub window_size: usize,
    pub get_changes: bool,
    pub filter_start: chrono::DateTime<Utc>,
    /// The sticky-resolved per-collection `<Options>` block
    /// ([MS-ASCMD] §2.2.3.125.6) driving body negotiation
    /// ([MS-ASAIRS] §2.2.2.12) for the items rendered in this response.
    pub body_options: crate::eas_sync_options::EasSyncCollectionOptions,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            window_size: DEFAULT_WINDOW_SIZE,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        }
    }
}

/// Whether a stored calendar event absent from the current JMAP response should
/// be tombstoned (deleted).
///
/// A `CalendarEvent/changes` delta is a PARTIAL change set: an event missing
/// from it is merely unchanged and must survive. Only a full snapshot (initial
/// sync, or the CalDAV fallback) is authoritative enough to treat an omission
/// as a deletion, and within a delta only an explicitly `destroyed` id is
/// removed.
fn calendar_event_deleted(
    delta_partial: bool,
    destroyed_server_ids: &HashSet<String>,
    server_id: &str,
) -> bool {
    !delta_partial || destroyed_server_ids.contains(server_id)
}

/// Sync-state token stored while a snapshot prime is still being delivered
/// in clipped windows: `prime-cursor:<last-delivered-href>`. The href
/// ordering matches the deterministic `events.sort_by(href)` delivery order,
/// so the continuation can resume strictly after the delivered prefix and —
/// crucially — force-deliver the remainder even when a shared `item_map`
/// row already exists (another device's completed prime may have populated
/// the map with events this device never saw). A JMAP delta token never
/// takes this shape, so `previous_state` is a prime continuation only when
/// it resolves under this prefix and the SyncKey matches (checked by the
/// caller before the prime/delta dispatch).
fn prime_cursor_token(last_href: &str) -> String {
    format!("prime-cursor:{last_href}")
}

fn prime_cursor_from_token(token: Option<&str>) -> Option<String> {
    token
        .and_then(|t| t.strip_prefix("prime-cursor:"))
        .filter(|h| !h.is_empty())
        .map(str::to_string)
}

/// Fetch the complete calendar snapshot for the wide prime window by paging
/// `CalendarEvent/query` with `position` until every reported id has been
/// seen. The page size is deliberately small (100) so a backend that caps
/// query results per page is drained incrementally rather than truncated.
/// The loop terminates when the accumulated distinct-id count reaches the
/// first page's `total`, when a page returns no event, or when a page
/// contributes zero NEW ids (the backend stopped making progress and the
/// caller-visible snapshot is incomplete — surfaced via `total > len`). The
/// delta `state`/`query_state` and the merged `total` are taken from the
/// FIRST page: they describe the snapshot as of the initial query and must
/// not drift across pages.
const PRIME_QUERY_PAGE_SIZE: u64 = 100;

async fn query_all_calendar_events(
    jmap: &crate::jmap::JmapClient,
    account_id: &str,
    calendar_id: Option<&str>,
    start: &str,
    end: &str,
    username: &str,
    password: &SecretString,
) -> Result<crate::jmap::CalendarEventListResult> {
    let first = jmap
        .query_calendar_events(crate::jmap::QueryCalendarEventsParams {
            account_id,
            calendar_id,
            start,
            end,
            position: 0,
            limit: PRIME_QUERY_PAGE_SIZE,
            username,
            password,
        })
        .await?;
    let total = first.total;
    let mut events = first.events;
    let mut seen: HashSet<String> = events.iter().filter_map(|e| e.id.clone()).collect();
    // Page while more ids remain behind the accumulated distinct set.
    loop {
        if (events.len() as u64) >= total {
            break;
        }
        let page = jmap
            .query_calendar_events(crate::jmap::QueryCalendarEventsParams {
                account_id,
                calendar_id,
                start,
                end,
                position: events.len() as u64,
                limit: PRIME_QUERY_PAGE_SIZE,
                username,
                password,
            })
            .await?;
        if page.events.is_empty() {
            break;
        }
        let new_ids: Vec<_> = page
            .events
            .into_iter()
            .filter(|e| e.id.as_ref().is_some_and(|id| seen.insert(id.clone())))
            .collect();
        if new_ids.is_empty() {
            // The backend stopped making progress: surface an incomplete
            // snapshot (`total > len`) so the caller never persists a
            // state that would skip the unseen events.
            break;
        }
        events.extend(new_ids);
        if (events.len() as u64) >= total {
            break;
        }
    }
    Ok(crate::jmap::CalendarEventListResult {
        events,
        total,
        query_state: first.query_state,
        state: first.state,
        unparseable: first.unparseable,
    })
}

pub async fn perform_sync(params: &PerformSyncParams<'_>) -> Result<String> {
    let storage = &params.state.storage;
    let window_size = params.opts.window_size.clamp(1, MAX_WINDOW_SIZE);

    if !params.content_class.eq_ignore_ascii_case("Calendar") {
        let class_name = params.content_class.trim();
        let normalized = if class_name.is_empty() {
            "Calendar"
        } else {
            class_name
        };
        let new_sync_key = Uuid::new_v4().to_string();
        storage
            .set_sync_key(
                params.owner,
                params.state_collection_id,
                &new_sync_key,
                Some("token"),
            )
            .await?;
        storage
            .record_journal_watermark(params.owner, params.state_collection_id)
            .await?;
        let pseudo_resource = format!(
            "class://{}/{}",
            params.owner,
            normalized.to_ascii_lowercase()
        );
        let server_id = generate_server_id(params.state.cfg.hmac_secret(), &pseudo_resource);
        let app_data = class_placeholder_app_data(normalized, params.owner);
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
<Sync xmlns="AirSync:" xmlns:Contacts="Contacts:" xmlns:Contacts2="Contacts2:" xmlns:Tasks="Tasks:" xmlns:Notes="Notes:" xmlns:DocumentLibrary="DocumentLibrary:" xmlns:RightsManagement="RightsManagement:" xmlns:AirSyncBase="AirSyncBase:">
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
            xml_escape(params.collection_id),
            params.client_mutation_responses,
            commands
        ));
    }

    let previous_state = storage
        .get_sync_key(params.owner, params.state_collection_id)
        .await?;
    if params.incoming_sync_key != "0" {
        match previous_state.as_ref() {
            Some((expected_sync_key, _)) if expected_sync_key == params.incoming_sync_key => {}
            _ => return Ok(invalid_sync_key_response(params.collection_id, "Calendar")),
        }
    }

    // The calendar delta is derived from the backend snapshot diffed against
    // `item_map` (etag comparison), never from the client's stored watermark —
    // so §16 journal pruning cannot cut a window out from under a calendar
    // client, and no stale-watermark guard applies here (the stored token is
    // frequently an opaque JMAP `query_state`, which would make a naive guard
    // fire forever).
    let _since = if params.incoming_sync_key == "0" {
        0
    } else {
        sync_since_from_token(
            previous_state
                .as_ref()
                .and_then(|(_, token)| token.as_deref()),
        )
    };

    if !params.opts.get_changes && params.incoming_sync_key != "0" {
        let latest_seq = storage.get_latest_change_seq().await.unwrap_or(0);
        let new_sync_key = Uuid::new_v4().to_string();
        storage
            .set_sync_key(
                params.owner,
                params.state_collection_id,
                &new_sync_key,
                Some(&sync_seq_to_token(latest_seq)),
            )
            .await?;
        storage
            .set_journal_watermark(params.owner, params.state_collection_id, latest_seq)
            .await?;
        return Ok(format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>Calendar</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>1</Status>
{}<Commands></Commands>
</Collection></Collections>
</Sync>"#,
            new_sync_key, params.collection_id, params.client_mutation_responses
        ));
    }

    let start_db_seq = storage.get_latest_change_seq().await.unwrap_or(0);

    // EventItem struct used by both JMAP and CalDAV paths
    #[derive(Clone)]
    struct EventItem {
        href: String,
        etag: String,
        ics: String,
        item: CalendarItem,
    }

    // Try JMAP Calendar first (urn:ietf:params:jmap:calendars).
    // Falls back to CalDAV if JMAP Calendar is unavailable or fails.
    let mut events = Vec::new();
    let mut jmap_failed = false;
    let mut token_to_store: Option<String> = None;
    // True when `events` is a partial JMAP delta (CalendarEvent/changes) rather
    // than a complete view of the calendar. Stored events absent from a partial
    // delta are merely unchanged and must NOT be tombstoned.
    let mut calendar_delta_partial = false;
    // Server IDs the delta explicitly reported as destroyed.
    let mut jmap_destroyed_server_ids: HashSet<String> = HashSet::new();
    // True when the JMAP drain stopped at its accumulation/page cap with
    // `hasMoreChanges` still set: the response must signal
    // `<MoreAvailable/>` even when it fits the EAS window, and the
    // intermediate state is persisted so the next delta drains the rest.
    let mut drain_has_more = false;
    // Delivery cursor for an in-progress snapshot prime: `Some(href)` when
    // `previous_state` carries a `prime-cursor:` token (per-device, since
    // `state_collection_id` is device-scoped). A prime continuation resumes
    // after this href and force-delivers the remainder.
    let prime_cursor: Option<String> =
        prime_cursor_from_token(previous_state.as_ref().and_then(|(_, t)| t.as_deref()));

    // Folder scoping: a user-created calendar-family folder (Types 13/15/16/17,
    // [MS-ASCMD] §2.2.3.186.2) has its CalDAV collection href as its EAS
    // CollectionId. Without scoping, the Calendar Sync path would query with
    // `calendar_id: None` (every calendar) and the CalDAV fallback would use
    // the FIRST calendar returned, so the client would receive events from
    // other calendars. Resolve the href to its JMAP calendar id and scope both
    // backends to it. `Some(None)` marks a custom folder whose calendar could
    // not be resolved: serve an empty collection rather than leaking the
    // default calendar's events.
    let mut scoped_calendar_href: Option<String> = None;
    let calendar_scope: Option<Option<String>> =
        match collection_folder_href(&params.state, params.owner, params.collection_id).await? {
            None => None,
            Some(href) => {
                let resolved = match &params.state.jmap_client {
                    Some(jmap) => {
                        let pw = SecretString::from(params.password.to_string());
                        jmap.resolve_calendar_id_for_href(params.username, &pw, &href)
                            .await
                            .unwrap_or(None)
                    }
                    None => None,
                };
                scoped_calendar_href = Some(href);
                Some(resolved)
            }
        };
    let scoped_calendar_id: Option<&str> = calendar_scope.as_ref().and_then(|s| s.as_deref());
    let scope_unresolved = matches!(calendar_scope, Some(None));

    if params.state.cfg.prefer_jmap_calendar && !scope_unresolved {
        if let Some(jmap) = &params.state.jmap_client {
            let password_secret = SecretString::from(params.password.to_string());
            if jmap
                .supports_calendar(params.username, &password_secret)
                .await
            {
                // Get JMAP Calendar account ID
                let account_id = match jmap
                    .get_calendar_account_id(params.username, &password_secret)
                    .await
                {
                    Ok(id) => id,
                    Err(e) => {
                        tracing::debug!(target: "sync", error = %e, "JMAP Calendar account ID lookup failed, falling back to CalDAV");
                        jmap_failed = true;
                        String::new()
                    }
                };
                if !jmap_failed {
                    // Resume token for a `CalendarEvent/changes` delta. `None`
                    // means "prime from a full snapshot": either SyncKey 0, a
                    // delta the server refused with `cannotCalculateChanges`
                    // (finding #7), or an in-progress windowed prime carrying a
                    // `prime-cursor:` token — the prime below then re-issues the
                    // query resuming past the cursor and stores the resulting
                    // state once the live set is fully delivered.
                    let resume: Option<String> =
                        if params.incoming_sync_key == "0" || prime_cursor.is_some() {
                            None
                        } else {
                            previous_state
                                .as_ref()
                                .and_then(|(_, token)| token.as_deref())
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                        };
                    let mut done = false;

                    if let Some(old_state_str) = resume.as_deref() {
                        // Delta sync: drain the whole `CalendarEvent/changes`
                        // chain (finding #5) so the stored token is the final
                        // `newState`, never an intermediate one whose remaining
                        // pages would be skipped.
                        match jmap
                            .accumulate_calendar_changes(
                                &account_id,
                                old_state_str,
                                params.username,
                                &password_secret,
                            )
                            .await
                        {
                            Ok(changes) => {
                                let mut all_ids: Vec<String> = changes
                                    .created
                                    .iter()
                                    .chain(changes.updated.iter())
                                    .cloned()
                                    .collect();
                                // An id can appear as created on one page and
                                // updated on another; fetch each once.
                                {
                                    let mut seen = HashSet::with_capacity(all_ids.len());
                                    all_ids.retain(|id| seen.insert(id.clone()));
                                }
                                // A delta is a PARTIAL change set: events
                                // absent from it are unchanged, not gone.
                                calendar_delta_partial = true;
                                // The `CalendarEvent/changes` feed is
                                // ACCOUNT-wide, so a destroyed id may belong to
                                // ANY calendar. `item_map` is owner-global (no
                                // collection column), so tombstoning a
                                // destroyed id here removes a SHARED row and
                                // emits a `<Delete>` in this collection for an
                                // event the client never held — and, by
                                // removing the row first, suppresses the
                                // Delete the event's OWN collection would
                                // otherwise deliver (a silently missed
                                // deletion). A scoped folder therefore does not
                                // infer deletions from the account feed; an
                                // event deleted from its own calendar lingers
                                // until the folder's next full re-prime. An
                                // unscoped (default) calendar folder owns the
                                // whole account feed and tombstones normally.
                                if scoped_calendar_id.is_none() {
                                    for id in &changes.destroyed {
                                        let href = format!("jmap://calendar/{}/{}", account_id, id);
                                        jmap_destroyed_server_ids.insert(generate_server_id(
                                            params.state.cfg.hmac_secret(),
                                            &href,
                                        ));
                                    }
                                }
                                let mut conversion_failed = false;
                                if !all_ids.is_empty() {
                                    match jmap
                                        .get_calendar_events(
                                            &account_id,
                                            &all_ids,
                                            scoped_calendar_id,
                                            params.username,
                                            &password_secret,
                                        )
                                        .await
                                    {
                                        Ok(event_map) => {
                                            for id in all_ids {
                                                match event_map.get(&id) {
                                                    Some((ics, etag)) => match parse_ics_event(ics)
                                                    {
                                                        Some(item) => {
                                                            events.push(EventItem {
                                                                href: format!(
                                                                    "jmap://calendar/{}/{}",
                                                                    account_id, id
                                                                ),
                                                                etag: etag.clone(),
                                                                ics: ics.clone(),
                                                                item,
                                                            });
                                                        }
                                                        None => {
                                                            conversion_failed = true;
                                                            tracing::warn!(
                                                                target: "sync",
                                                                event_id = %id,
                                                                "changed JMAP calendar event could not be parsed; not advancing sync state"
                                                            );
                                                        }
                                                    },
                                                    None => {
                                                        // A scoped folder's delta is
                                                        // the account-wide change feed;
                                                        // an id belonging to another
                                                        // calendar is filtered out by
                                                        // `get_calendar_events` and is
                                                        // not a failure for this folder.
                                                        // Only an unscoped fetch treats
                                                        // an absent id as a fetch failure.
                                                        if scoped_calendar_id.is_none() {
                                                            conversion_failed = true;
                                                            tracing::warn!(
                                                                target: "sync",
                                                                event_id = %id,
                                                                "changed JMAP calendar event could not be fetched; not advancing sync state"
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::debug!(target: "sync", error = %e, "Failed to fetch changed calendar events from JMAP, falling back to CalDAV");
                                            jmap_failed = true;
                                        }
                                    }
                                }
                                if !jmap_failed && conversion_failed {
                                    // The delta named events we could not turn
                                    // into items; storing its state would skip
                                    // them forever. Re-read via CalDAV instead.
                                    jmap_failed = true;
                                }
                                if !jmap_failed {
                                    drain_has_more = changes.has_more_changes;
                                    token_to_store = Some(changes.new_state);
                                    done = true;
                                }
                            }
                            Err(e) => {
                                if e.downcast_ref::<crate::jmap::CannotCalculateChanges>()
                                    .is_some()
                                {
                                    // The server no longer has the state we
                                    // stored; fall through to a full re-prime
                                    // rather than erroring out to CalDAV.
                                    tracing::warn!(
                                        target: "sync",
                                        error = %e,
                                        "JMAP CalendarEvent/changes cannot calculate changes; re-priming from a full query"
                                    );
                                } else {
                                    tracing::debug!(target: "sync", error = %e, "JMAP Calendar changes failed, falling back to CalDAV");
                                    jmap_failed = true;
                                }
                            }
                        }
                    }

                    if !jmap_failed && !done {
                        // Initial full sync (or re-prime after
                        // `cannotCalculateChanges`) with a wide time window.
                        let start = params
                            .opts
                            .filter_start
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string();
                        let end = (Utc::now() + Duration::weeks(104))
                            .format("%Y-%m-%dT%H:%M:%SZ")
                            .to_string();
                        match query_all_calendar_events(
                            jmap,
                            &account_id,
                            scoped_calendar_id,
                            &start,
                            &end,
                            params.username,
                            &password_secret,
                        )
                        .await
                        {
                            Ok(result) => {
                                let mut conversion_failed = false;
                                for event in &result.events {
                                    match event.to_ics().and_then(|ics| {
                                        parse_ics_event(&ics).map(|item| (ics, item))
                                    }) {
                                        Some((ics, item)) => {
                                            let href = format!(
                                                "jmap://calendar/{}/{}",
                                                account_id,
                                                event.id.as_deref().unwrap_or("")
                                            );
                                            // A stable content-addressed token:
                                            // Stalwart exposes no `@etag` for
                                            // calendar events (finding #6), so
                                            // the server value wins when present
                                            // and a SHA-256 of the event object
                                            // is used otherwise.
                                            let etag = match event.raw.as_ref() {
                                                Some(raw) => crate::jmap::calendar_event_etag(
                                                    raw,
                                                    event.etag.as_deref(),
                                                ),
                                                None => event.etag.clone().unwrap_or_default(),
                                            };
                                            events.push(EventItem {
                                                href,
                                                etag,
                                                ics: ics.clone(),
                                                item,
                                            });
                                        }
                                        None => {
                                            conversion_failed = true;
                                            tracing::warn!(
                                                target: "sync",
                                                event_id = event.id.as_deref().unwrap_or(""),
                                                "JMAP calendar event could not be converted or parsed; not advancing sync state"
                                            );
                                        }
                                    }
                                }
                                if conversion_failed || result.unparseable > 0 {
                                    // Do not store a state token that would
                                    // skip the unprocessed event on the next
                                    // delta; re-query the full set instead.
                                    jmap_failed = true;
                                } else if result.total > result.events.len() as u64 {
                                    // `query_all_calendar_events` paged by
                                    // `position` until the snapshot was
                                    // exhausted, so `total > len` here means
                                    // the pages stopped making progress (a
                                    // new id never appeared) — an incomplete
                                    // snapshot. Never store its state: the
                                    // next Sync re-primes from the start, and
                                    // the partial mark keeps the tombstone
                                    // sweep from deleting unseen events.
                                    tracing::warn!(
                                        target: "sync",
                                        total = result.total,
                                        fetched = result.events.len(),
                                        "JMAP calendar prime snapshot incomplete; re-priming next Sync"
                                    );
                                    calendar_delta_partial = true;
                                } else {
                                    // Store the `CalendarEvent/get` data-type
                                    // `state`, which is what
                                    // `CalendarEvent/changes` accepts as
                                    // `sinceState` (finding #7). The query's
                                    // `queryState` is a different state space
                                    // and is only a fallback when the server
                                    // omitted the data-type state.
                                    let token = if result.state.is_empty() {
                                        result.query_state.clone()
                                    } else {
                                        result.state.clone()
                                    };
                                    token_to_store = Some(token);
                                }
                            }
                            Err(e) => {
                                tracing::debug!(target: "sync", error = %e, "JMAP Calendar query failed, falling back to CalDAV");
                                jmap_failed = true;
                            }
                        }
                    }
                }
            } else {
                jmap_failed = true;
            }
        } else {
            jmap_failed = true;
        }
    } else {
        // Two cases reach here and BOTH must fall back to CalDAV:
        //  * CalDAV-only mode (`prefer_jmap_calendar` false) — JMAP is not the
        //    calendar backend;
        //  * a custom folder whose calendar scope could not be resolved
        //    (`scope_unresolved`) — serving an empty snapshot instead would be
        //    marked authoritative and the tombstone sweep below would delete
        //    every stored calendar event. Falling back queries the folder's own
        //    CalDAV collection (when known) or the default calendar.
        jmap_failed = true;
    }
    // Fallback to CalDAV only if JMAP Calendar failed (not if it returned zero events)
    // Zero events is a valid JMAP result; falling back would cause unnecessary
    // CalDAV requests and potential stale/duplicate data.
    if jmap_failed {
        events.clear();
        // The CalDAV fallback returns a complete snapshot of the collection, so
        // it is authoritative: a stored event it omits is genuinely deleted.
        calendar_delta_partial = false;
        jmap_destroyed_server_ids.clear();
        let caldav = CaldavClient::new(&params.state.cfg)?;
        // Prefer the folder's own collection (resolved from its stored href)
        // over the first calendar returned: a custom calendar folder must sync
        // only its own collection. `absolute_url` keeps the configured base
        // path, so the stored href resolves to the collection that
        // `ensure_calendar_collection` created.
        let collection_href = match scoped_calendar_href.as_deref() {
            Some(href) => caldav.absolute_url(href)?,
            None => {
                let calendars = caldav
                    .find_user_calendars(params.username, params.password)
                    .await?;
                calendars
                    .first()
                    .ok_or_else(|| anyhow!("no calendars found"))?
                    .clone()
            }
        };
        let start = params
            .opts
            .filter_start
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let end = (Utc::now() + Duration::weeks(104))
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        let events_xml = caldav
            .query_events(
                &collection_href,
                &start,
                &end,
                params.username,
                params.password,
            )
            .await?;

        use quick_xml::Reader;
        use quick_xml::events::Event;

        let mut reader = Reader::from_str(&events_xml);
        reader.config_mut().trim_text(false);
        let mut current = EventItem {
            href: String::new(),
            etag: String::new(),
            ics: String::new(),
            item: CalendarItem::default(),
        };
        let mut buf = Vec::new();
        let mut in_caldata = false;
        let mut in_href = false;
        let mut in_getetag = false;
        let mut caldata_buf = String::new();

        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(ref e)) => match e.name().local_name().as_ref() {
                    "href" => {
                        in_href = true;
                        current.href.clear();
                    }
                    "getetag" => {
                        in_getetag = true;
                        current.etag.clear();
                    }
                    "calendar-data" => {
                        in_caldata = true;
                        caldata_buf.clear();
                    }
                    _ => {}
                },
                Ok(Event::Text(ref e)) if in_href => {
                    current.href.push_str(e.as_ref());
                }
                Ok(Event::GeneralRef(ref r)) if in_href => {
                    current.href.push_str(&resolve_xml_reference(r.as_ref()));
                }
                Ok(Event::Text(ref e)) if in_getetag => {
                    current.etag.push_str(e.as_ref());
                }
                Ok(Event::GeneralRef(ref r)) if in_getetag => {
                    current.etag.push_str(&resolve_xml_reference(r.as_ref()));
                }
                Ok(Event::Text(ref e)) if in_caldata => {
                    caldata_buf.push_str(e);
                }
                Ok(Event::CData(ref e)) if in_caldata => {
                    caldata_buf.push_str(e.as_ref());
                }
                Ok(Event::GeneralRef(ref r)) if in_caldata => {
                    caldata_buf.push_str(&resolve_xml_reference(r.as_ref()));
                }
                Ok(Event::End(ref e)) => match e.name().local_name().as_ref() {
                    "href" => {
                        in_href = false;
                        current.href = current.href.trim().to_string();
                    }
                    "getetag" => {
                        in_getetag = false;
                        current.etag = current.etag.trim().to_string();
                    }
                    "calendar-data" if in_caldata => {
                        in_caldata = false;
                        current.ics = caldata_buf.trim().to_string();
                    }
                    "response" => {
                        // Skip the calendar collection itself (which has no calendar-data)
                        // Only process individual events that have ICS data
                        if !current.href.is_empty()
                            && !current.ics.is_empty()
                            && let Some(item) = parse_ics_event(&current.ics)
                        {
                            current.item = item;
                            events.push(current.clone());
                        } else if !current.href.is_empty() && current.ics.is_empty() {
                            tracing::debug!(
                                "sync: skipping href {} (no calendar-data element - likely calendar collection root)",
                                current.href
                            );
                        }
                        current = EventItem {
                            href: String::new(),
                            etag: String::new(),
                            ics: String::new(),
                            item: CalendarItem::default(),
                        };
                    }
                    _ => {}
                },
                Ok(Event::Eof) => break,
                _ => {}
            }
            buf.clear();
        }
    }

    // Deterministic delivery order: the EAS response window is applied to
    // `pending_items` in vector order, so a stable sort makes a clipped prime
    // deliver a contiguous, stable prefix of the set across successive Syncs
    // (server_id is derived from the href, so ordering by href is equivalent).
    // Delta results (already a bounded change set) are unaffected in practice.
    events.sort_by(|a, b| a.href.cmp(&b.href));

    // Per-scope delivery ledger (device+collection SyncKey stream) drives the
    // EAS Add/Change decision and the ETag-change filter for CALENDAR. `item_map`
    // is owner-global and records that SOME collection delivered an item; using
    // it here makes a fresh prime emit `<Change>` for an item this scope never
    // held (e.g. a second device, or an event shared by two calendars), and
    // makes a per-collection edit invisible to the other collection because the
    // shared ETag already matched. This scope is exactly one EAS SyncKey stream
    // (`state_collection_id`), whose SyncKey 0 prime the client treats as "forget
    // everything".
    //
    // `existing_map` (owner-global `item_map`) remains the candidate set for the
    // owner-level DELETION sweep below: a deletion is an account-level fact and
    // the shared row is what the delete fill removes once its `<Delete>` is
    // emitted.
    let existing_map = storage
        .list_ews_items(params.owner, 4096, 0)
        .await?
        .into_iter()
        .map(|item| (item.server_id.clone(), item))
        .collect::<HashMap<_, _>>();

    let initial_sync = params.incoming_sync_key == "0";
    let mut delivered_map = storage
        .list_calendar_deliveries(params.owner, params.state_collection_id)
        .await?;
    if initial_sync {
        // A fresh prime restarts the stream: the client was told to forget all
        // prior items, so within THIS response every event is an `<Add>`. The
        // in-memory ledger is emptied for the Add/Change decision here; the
        // durable ledger is cleared in `commit_calendar_sync` (atomic with the
        // new SyncKey). `item_map` is untouched (other scopes share it).
        delivered_map.clear();
    }
    // NOTE: an empty ledger on a non-prime (delta) is deliberately NOT seeded
    // from the owner-global `item_map`. A shared ETag would make this scope skip
    // a changed event another collection already delivered (its ETag "already
    // matches"), i.e. exactly the per-collection bug this ledger exists to fix.
    // With no ledger row, every event in this stream's change set is treated as
    // new to the stream and delivered as `<Add>` — EAS keys items by ServerId, so
    // an `<Add>` for an item the client already holds updates it in place. This is
    // the upgrade path: a scope's first delta after the ledger is introduced
    // re-delivers its bounded change set (and a fresh prime delivers the whole
    // snapshot) as `<Add>` rather than silently dropping events.

    struct PendingItem {
        server_id: String,
        resource_href: String,
        uid: String,
        etag: String,
        item: CalendarItem,
        is_add: bool,
    }
    let mut pending_items: Vec<PendingItem> = Vec::new();
    let mut pending_deletes: Vec<String> = Vec::new();
    let mut seen_ids = HashSet::new();
    let in_prime_cursor = prime_cursor.is_some();

    for ev in &events {
        if ev.href.is_empty() {
            continue;
        }
        let server_id = generate_server_id(params.state.cfg.hmac_secret(), &ev.href);
        seen_ids.insert(server_id.clone());
        let etag = ev.etag.trim_matches('"').to_string();
        // Use pre-parsed item from EventItem (set by both JMAP and CalDAV paths)
        let item = &ev.item;
        // Add vs Change is decided by THIS scope's ledger, not the owner-global
        // item_map: an item this exact SyncKey stream never delivered is an
        // `<Add>` even when another device/collection already stored it.
        let delivered_etag = delivered_map.get(&server_id);
        let is_add = delivered_etag.is_none();
        let changed = delivered_etag.map(|e| e != &etag).unwrap_or(true);
        // A prime continuation resumes strictly after the stored href
        // cursor: events at/before it were already delivered to THIS device,
        // so they are skipped unconditionally. Events past it are force-
        // delivered even when this scope's ledger already holds them with a
        // matching etag (a ledger write may lag the delivered window).
        if in_prime_cursor {
            if ev.href.as_str() <= prime_cursor.as_deref().unwrap() {
                continue;
            }
        } else if !initial_sync && !changed {
            continue;
        }
        pending_items.push(PendingItem {
            server_id,
            resource_href: ev.href.clone(),
            uid: item.uid.clone(),
            etag,
            item: item.clone(),
            is_add,
        });
    }

    for server_id in existing_map.keys() {
        if seen_ids.contains(server_id) {
            continue;
        }
        // A JMAP delta (CalendarEvent/changes) is a PARTIAL change set: a
        // stored event absent from it is merely unchanged and must survive.
        // Only an event the delta explicitly reported as destroyed is removed
        // here. A full snapshot (initial sync or the CalDAV fallback) is
        // authoritative, so any stored event it omits is a deletion.
        //
        // For a SCOPED folder (a custom calendar folder) that authority does
        // not extend to events outside its own calendar: `item_map` is
        // owner-global, so a snapshot of one calendar is not a statement about
        // another calendar's events. Sweeping them here would tombstone a
        // foreign calendar's live event in this collection (and remove the
        // shared row before its own collection could deliver it). Skip the
        // sweep entirely for scoped folders — their own deletions still arrive
        // as explicit deltas once their calendar reports them.
        let deleted = scoped_calendar_id.is_none()
            && calendar_event_deleted(
                calendar_delta_partial,
                &jmap_destroyed_server_ids,
                server_id,
            );
        if deleted {
            // Queue the tombstone now (it carries the journal seq and
            // must survive a clipped window) but RETAIN the item_map row:
            // removing it before its `<Delete>` is emitted would make the
            // held-token retry miss the journal row (`start_db_seq` has
            // moved past it) and the client would keep the deleted
            // meeting indefinitely. The row is dropped by the delete fill
            // only once its `<Delete>` command has been emitted.
            let _ = storage.add_delete_tombstone(params.owner, server_id).await;
        }
    }

    if !initial_sync {
        let tombstones = storage
            .list_deleted_since_seq(params.owner, start_db_seq)
            .await?;
        for (_, sid) in tombstones {
            if !seen_ids.contains(sid.as_str()) {
                pending_deletes.push(sid);
            }
        }
    }

    let mut commands = String::with_capacity(
        (pending_items.len() + pending_deletes.len()).min(window_size) * 4096,
    );
    let mut items_included = 0usize;
    let mut more_available = false;
    // Href of the last item actually delivered, for the prime cursor.
    let mut last_delivered_href: Option<String> = None;
    // Durable side effects of THIS response, materialised only once the whole
    // window has been rendered and committed atomically below.
    let mut delivered_now: Vec<CalendarDeliveryWrite<'_>> = Vec::new();

    for pi in &pending_items {
        if items_included >= window_size {
            more_available = true;
            break;
        }
        last_delivered_href = Some(pi.resource_href.clone());
        // Deferred to the atomic commit: recording delivery (item_map + this
        // scope's ledger) BEFORE the SyncKey advances would make a partial
        // failure lose the event on retry (the ledger would already mark it
        // delivered while the client never received the new SyncKey).
        delivered_now.push(CalendarDeliveryWrite {
            server_id: pi.server_id.as_str(),
            resource_href: pi.resource_href.as_str(),
            uid: pi.uid.as_str(),
            etag: pi.etag.as_str(),
        });
        if pi.is_add {
            let mut app_data =
                render_calendar_app_data_with_options(&pi.item, &params.opts.body_options);
            if let Ok(att_list) = params
                .state
                .attachment_manager
                .get_attachments_for_item(params.owner, &pi.server_id)
                .await
                && !att_list.is_empty()
            {
                let summaries: Vec<_> = att_list.iter().map(|a| a.to_eas_summary()).collect();
                app_data.push_str(&crate::attachment::render_eas_attachments_xml(&summaries));
            }
            commands.push_str(&format!(
                "<Add><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Add>",
                pi.server_id, app_data
            ));
        } else {
            let mut app_data =
                render_calendar_app_data_with_options(&pi.item, &params.opts.body_options);
            if let Ok(att_list) = params
                .state
                .attachment_manager
                .get_attachments_for_item(params.owner, &pi.server_id)
                .await
                && !att_list.is_empty()
            {
                let summaries: Vec<_> = att_list.iter().map(|a| a.to_eas_summary()).collect();
                app_data.push_str(&crate::attachment::render_eas_attachments_xml(&summaries));
            }
            commands.push_str(&format!(
                "<Change><ServerId>{}</ServerId><ApplicationData>{}</ApplicationData></Change>",
                pi.server_id, app_data
            ));
        }
        items_included += 1;
    }

    let mut deleted_now: Vec<&str> = Vec::new();
    if !more_available {
        for server_id in &pending_deletes {
            if items_included >= window_size {
                more_available = true;
                break;
            }
            commands.push_str(&format!(
                "<Delete><ServerId>{}</ServerId></Delete>",
                xml_escape(server_id)
            ));
            items_included += 1;
            // The push of this delete above is the moment the item_map row may
            // finally be removed. Deleting it any earlier (e.g. in the sweep
            // above) would lose the retry path: a clipped window that has not
            // emitted this `<Delete>` yet must keep the row so the held-token
            // retry re-queues it. The removal (item_map + THIS scope's ledger
            // row) is applied in the atomic commit below, together with the
            // new SyncKey, so a failed commit retains both the row and the
            // client's old SyncKey — the retry re-emits the `<Delete>`.
            deleted_now.push(server_id.as_str());
        }
    }

    let new_sync_key = Uuid::new_v4().to_string();
    // Advance the provider token only when the whole change set was delivered.
    // A clipped EAS window (MoreAvailable) leaves undelivered items behind
    // the current token; storing the new state there would make the next
    // delta start AFTER them and lose those events. Instead:
    // - a clipped DELTA keeps the previous token, so the next Sync re-derives
    //   the same change set (delivered items are filtered by the item_map
    //   etag comparison);
    // - a clipped SNAPSHOT (initial sync, re-prime, or CalDAV fallback
    //   snapshot) stores a `prime-cursor:` token naming the last delivered
    //   href in delivery order, so the continuation re-primes from that
    //   cursor — never as a delta from a stale provider token, and with
    //   forced delivery past the cursor (see `in_prime_cursor`);
    // - a drain that stopped at its cap with `hasMoreChanges` set also
    //   signals `<MoreAvailable/>`, but the accumulated changes were
    //   delivered, so the intermediate state IS persisted and the next delta
    //   resumes from `newState`.
    let provider_token = token_to_store;
    // An incomplete JMAP prime snapshot (query pages stopped making
    // progress: `calendar_delta_partial` set with no provider token) must
    // store NO token at all, so the next Sync re-primes from the full
    // snapshot instead of attempting a delta from a state that skips the
    // unseen events.
    let incomplete_prime = calendar_delta_partial && provider_token.is_none();
    let token_val: Option<String> = if more_available {
        if calendar_delta_partial && provider_token.is_some() {
            // Clipped JMAP delta (or a clipped delta that drained the chain
            // fully): hold the previous token so the next Sync re-derives
            // the same change set.
            previous_state.as_ref().and_then(|(_, t)| t.clone())
        } else {
            // Clipped snapshot (initial sync, re-prime, or CalDAV
            // fallback snapshot): store a prime cursor over the delivered
            // prefix (falling back to the pre-sync start when nothing was
            // delivered, which re-primes from the beginning).
            let cursor = match last_delivered_href.as_deref().or(prime_cursor.as_deref()) {
                Some(href) => prime_cursor_token(href),
                None => sync_seq_to_token(start_db_seq),
            };
            Some(cursor)
        }
    } else if drain_has_more {
        Some(provider_token.unwrap_or_else(|| sync_seq_to_token(start_db_seq)))
    } else if incomplete_prime {
        None
    } else {
        Some(provider_token.unwrap_or_else(|| sync_seq_to_token(start_db_seq)))
    };
    // Journal watermark independent of the provider token (which may be a
    // non-`seq:` JMAP query_state): everything journaled up to the delivered
    // set is consumed. When the window clipped (MoreAvailable), undelivered
    // rows must stay visible to Ping, so hold the watermark at the pre-sync
    // head (`Some`); the client's obligatory follow-up Sync drains the
    // remainder. A full drain (`None`) lets the commit read the journal head
    // AFTER its own delivery rows are written, so the device is never woken by
    // the very rows it just delivered.
    let watermark_hold = more_available.then_some(start_db_seq);
    // Commit every durable side effect of this response in ONE transaction:
    // the delivered items' item_map + change_journal rows and per-scope ledger
    // rows, the emitted `<Delete>`s' removals, the new SyncKey + token, and the
    // watermark. They MUST advance together — writing the ledger before the
    // SyncKey would let a later failure lose an event on the client's retry
    // (the ledger would mark it delivered while the client never got the new
    // SyncKey).
    storage
        .commit_calendar_sync(
            params.owner,
            params.state_collection_id,
            &new_sync_key,
            token_val.as_deref(),
            watermark_hold,
            initial_sync,
            &delivered_now,
            &deleted_now,
        )
        .await?;
    let more_available_tag = if more_available || drain_has_more {
        "<MoreAvailable/>"
    } else {
        ""
    };
    let collection_id_escaped = xml_escape(params.collection_id);

    Ok(format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>Calendar</Class>
<SyncKey>{sync_key}</SyncKey>
<CollectionId>{collection_id}</CollectionId>
<Status>1</Status>
{more}{responses}<Commands>{commands}</Commands>
</Collection></Collections>
</Sync>"#,
        sync_key = new_sync_key,
        collection_id = collection_id_escaped,
        responses = params.client_mutation_responses,
        more = more_available_tag,
        commands = commands,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn tombstone_only_when_snapshot_or_explicitly_destroyed() {
        // Full snapshot (initial sync or CalDAV fallback): an omitted stored
        // event is genuinely deleted.
        assert!(calendar_event_deleted(false, &HashSet::new(), "sid-1"));
        // Partial delta: an event merely absent from the change set survives.
        assert!(!calendar_event_deleted(true, &HashSet::new(), "sid-1"));
        // Partial delta: an explicitly destroyed event is removed.
        let mut destroyed = HashSet::new();
        destroyed.insert("sid-2".to_string());
        assert!(calendar_event_deleted(true, &destroyed, "sid-2"));
        assert!(!calendar_event_deleted(true, &destroyed, "sid-3"));
    }

    fn eastern_item() -> CalendarItem {
        CalendarItem {
            uid: "eas-tz-001".to_string(),
            subject: "Eastern Meeting".to_string(),
            // 09:00 EST = 14:00 UTC on 2025-03-02 (before DST).
            start: chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap(),
            end: chrono::Utc.with_ymd_and_hms(2025, 3, 2, 15, 0, 0).unwrap(),
            all_day: false,
            dtstamp: Some(chrono::Utc.with_ymd_and_hms(2025, 3, 2, 14, 0, 0).unwrap()),
            timezone: Some("America/New_York".to_string()),
            ..Default::default()
        }
    }

    /// [MS-ASWBXML] code page 4 (Calendar) has no token slots for
    /// <Calendar:StartTimeZone>/<EndTimeZone> — the only time-zone
    /// representation on the wire is the <Calendar:Timezone> byte-array
    /// blob ([MS-ASCAL] §2.2.3.9). The renderer MUST NOT emit
    /// StartTimeZone/EndTimeZone at all: in a WBXML response they could
    /// only appear as literal inline text, which the target clients do not
    /// parse as structured data. The Timezone blob itself stays mandatory
    /// for non-UTC events and must carry the correct Eastern bias.
    #[test]
    fn eas_calendar_never_emits_startendtimezone_only_timezone_blob() {
        let item = eastern_item();
        let options = crate::eas_sync_options::EasSyncCollectionOptions::default();
        let xml = render_calendar_app_data_with_options(&item, &options);
        assert!(
            !xml.contains("StartTimeZone"),
            "StartTimeZone must never be emitted (no code-page-4 token):\n{xml}"
        );
        assert!(
            !xml.contains("EndTimeZone"),
            "EndTimeZone must never be emitted (no code-page-4 token):\n{xml}"
        );
        // The authoritative zone carrier is still present as a real base64
        // TZI blob with the correct Eastern standard bias.
        let blob = extract_element_text(&xml, "Calendar:Timezone").unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(blob.trim())
            .expect("Timezone is not valid base64");
        assert_eq!(
            bytes.len(),
            crate::timezone::TZ_BLOB_LEN,
            "EAS timezone blob is not the documented TZ_BLOB_LEN"
        );
        let bias = i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        assert_eq!(bias, 300, "Eastern standard bias = +300");
    }

    /// §14 wire-level proof: the rendered `<Calendar:Timezone>` base64 blob
    /// is the authoritative zone carrier for EAS, so it must round-trip —
    /// decode back to the SAME IANA id the item stores, and carry the full
    /// DST rules, not just a bias: a January and a July wall clock evaluated
    /// through the blob must reproduce `chrono_tz`'s ground-truth offsets
    /// (which differ by the DST delta for DST zones).
    #[test]
    fn eas_calendar_timezone_blob_round_trips_zone_and_dst() {
        use chrono::NaiveDate;
        use chrono::Offset;
        use chrono_tz::Tz;

        for zone in [
            "America/Los_Angeles",
            "America/New_York",
            "Europe/Berlin",
            "Australia/Sydney",
            "Asia/Kolkata",
        ] {
            let tz: Tz = zone.parse().expect("valid IANA id");
            let item = CalendarItem {
                uid: format!("eas-tz-{zone}"),
                subject: "TZ round trip".to_string(),
                start: chrono::Utc.with_ymd_and_hms(2025, 1, 15, 12, 0, 0).unwrap(),
                end: chrono::Utc.with_ymd_and_hms(2025, 1, 15, 13, 0, 0).unwrap(),
                all_day: false,
                timezone: Some(zone.to_string()),
                ..Default::default()
            };
            let options = crate::eas_sync_options::EasSyncCollectionOptions::default();
            let xml = render_calendar_app_data_with_options(&item, &options);
            let blob_b64 = extract_element_text(&xml, "Calendar:Timezone")
                .unwrap_or_else(|| panic!("{zone}: no Timezone blob rendered:\n{xml}"));
            let iana = crate::timezone::eas_timezone_blob_to_iana(&blob_b64)
                .unwrap_or_else(|| panic!("{zone}: blob decodes to no IANA id"));
            assert_eq!(iana, zone, "{zone}: blob round-trip mismatch");

            let blob = crate::timezone::decode_eas_timezone_blob(&blob_b64)
                .unwrap_or_else(|| panic!("{zone}: blob does not decode"));
            for (month, day) in [(1, 15), (7, 15)] {
                let naive = NaiveDate::from_ymd_opt(2025, month, day)
                    .and_then(|d| d.and_hms_opt(12, 0, 0))
                    .expect("sample date");
                let blob_minutes = crate::timezone::tzi_offset_minutes_at(&blob, naive);
                let chrono_minutes = naive
                    .and_local_timezone(tz)
                    .earliest()
                    .map(|dt| dt.offset().fix().local_minus_utc() / 60)
                    .unwrap_or_else(|| panic!("{zone}: {naive} has no chrono mapping"));
                assert_eq!(
                    blob_minutes, chrono_minutes,
                    "{zone}: {month:02}-{day:02} blob/chrono offset drift"
                );
            }
        }
    }

    /// A UTC event (no stored zone) omits the <Calendar:Timezone> blob
    /// entirely — EAS clients treat a UTC event's times as absolute
    /// instants, and the blob's all-zero transitions read as a failure to
    /// some clients.
    #[test]
    fn eas_calendar_omits_timezone_blob_for_utc_event() {
        let item = CalendarItem {
            uid: "utc-001".to_string(),
            subject: "UTC Meeting".to_string(),
            start: chrono::Utc.with_ymd_and_hms(2025, 1, 1, 9, 0, 0).unwrap(),
            end: chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap(),
            all_day: false,
            timezone: None,
            ..Default::default()
        };
        let options = crate::eas_sync_options::EasSyncCollectionOptions::default();
        let xml = render_calendar_app_data_with_options(&item, &options);
        assert!(
            !xml.contains("<Calendar:Timezone>"),
            "UTC event should not carry a Timezone blob:\n{xml}"
        );
        assert!(
            !xml.contains("StartTimeZone"),
            "UTC event should not carry a StartTimeZone:\n{xml}"
        );
    }

    fn extract_element_text(xml: &str, tag: &str) -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml.find(&open)? + open.len();
        let end = xml[start..].find(&close)? + start;
        Some(xml[start..end].to_string())
    }

    /// Collection-scoped Options apply to exception bodies exactly as to the
    /// parent item's body ([MS-ASCMD] §2.2.3.125.6): a negotiated Type 2
    /// (HTML) preference must wrap the exception description in the
    /// minimal-HTML document, and a TruncationSize must cut the exception
    /// Data too.
    #[test]
    fn eas_calendar_exception_body_honors_negotiated_options() {
        use crate::eas_sync_options::{EasBodyPreference, EasSyncCollectionOptions};

        let item = CalendarItem {
            exceptions: vec![crate::calendar::CalendarException {
                exception_start: chrono::Utc.with_ymd_and_hms(2025, 3, 9, 14, 0, 0).unwrap(),
                deleted: false,
                subject: Some("Moved meeting".to_string()),
                description: Some("Exception description body".to_string()),
                ..Default::default()
            }],
            ..eastern_item()
        };
        let options = EasSyncCollectionOptions {
            body_preferences: vec![EasBodyPreference {
                body_type: 2,
                truncation_size: Some(1024),
                ..Default::default()
            }],
            ..Default::default()
        };
        let xml = render_calendar_app_data_with_options(&item, &options);
        let exception =
            extract_element_text(&xml, "Calendar:Exception").expect("no exception rendered");
        assert!(
            exception.contains("<AirSyncBase:Type>2</AirSyncBase:Type>"),
            "exception body must honor the negotiated HTML preference:\n{exception}"
        );
        assert!(
            exception.contains("&lt;html&gt;&lt;body&gt;&lt;div&gt;Exception description body"),
            "exception body must be served as a minimal HTML document (XML-escaped):\n{exception}"
        );
        // And with no preferences the native plain text is served.
        let options = crate::eas_sync_options::EasSyncCollectionOptions::default();
        let xml = render_calendar_app_data_with_options(&item, &options);
        let exception =
            extract_element_text(&xml, "Calendar:Exception").expect("no exception rendered");
        assert!(
            exception.contains("<AirSyncBase:Type>1</AirSyncBase:Type>"),
            "default options must serve the native plain text:\n{exception}"
        );
    }

    /// A Type 2 (HTML) preference that fits the plain description can be
    /// AllOrNone-skipped once the minimal-HTML wrapper grows it past the
    /// limit: [MS-ASAIRS] §2.2.2.3.2 forbids serving the truncated body, so
    /// the re-negotiation must withhold the Data — and if a Type 1 fallback
    /// exists in the chain, the served format must switch to it with the
    /// Type 1 limit, never an HTML body truncated at the plain limit.
    #[test]
    fn eas_calendar_all_or_none_uses_converted_body_size() {
        use crate::eas_sync_options::{EasBodyPreference, EasSyncCollectionOptions};

        let mut item = CalendarItem {
            uid: "aon-001".to_string(),
            subject: "Review".to_string(),
            start: chrono::Utc.with_ymd_and_hms(2025, 3, 4, 9, 0, 0).unwrap(),
            end: chrono::Utc.with_ymd_and_hms(2025, 3, 4, 10, 0, 0).unwrap(),
            ..Default::default()
        };
        // The plain description is 100 bytes, so the first negotiation picks
        // the Type 2 preference (100 <= 120); plain_to_minimal_html grows it
        // past 120, so the re-negotiation skips it.
        let description = "d".repeat(100);
        let converted_len = crate::email::plain_to_minimal_html(&description).len();
        assert!(
            converted_len > 120,
            "fixture must convert past the all-or-none limit: {converted_len}"
        );
        item.description.clone_from(&description);

        // No fallback: the client forbade truncated HTML, so no Data.
        let options = EasSyncCollectionOptions {
            body_preferences: vec![EasBodyPreference {
                body_type: 2,
                truncation_size: Some(120),
                all_or_none: Some(true),
                ..Default::default()
            }],
            ..Default::default()
        };
        let xml = render_calendar_body_xml(&item.description, &options);
        assert!(
            xml.contains("<AirSyncBase:Type>1</AirSyncBase:Type>"),
            "the withheld announcement must carry the native type:\n{xml}"
        );
        assert!(
            !xml.contains("<AirSyncBase:Data>"),
            "AllOrNone must withhold the Data of a converted body that grew past the limit:\n{xml}"
        );
        assert!(
            !xml.contains("<AirSyncBase:Truncated>1"),
            "nothing was served, so Truncated must be absent:\n{xml}"
        );

        // With a truncatable Type 1 fallback the chain falls back to plain
        // text at the TYPE 1 limit — the HTML conversion is abandoned.
        let options = EasSyncCollectionOptions {
            body_preferences: vec![
                EasBodyPreference {
                    body_type: 2,
                    truncation_size: Some(120),
                    all_or_none: Some(true),
                    ..Default::default()
                },
                EasBodyPreference {
                    body_type: 1,
                    truncation_size: Some(50),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let xml = render_calendar_body_xml(&item.description, &options);
        assert!(
            xml.contains("<AirSyncBase:Type>1</AirSyncBase:Type>"),
            "fallback must serve the native plain type after the Type 2 skip:\n{xml}"
        );
        assert!(
            xml.matches('d').count() >= 50,
            "the plain fallback serves the description itself:\n{xml}"
        );
        assert!(
            xml.contains("<AirSyncBase:Truncated>1"),
            "the 100-byte description cut at the Type 1 limit of 50 is truncated:\n{xml}"
        );
    }

    #[test]
    fn eas_calendar_omits_bonus_timezone_elements_for_explicit_utc_zone() {
        // An event whose stored timezone is an explicit UTC id (not just an
        // absent `timezone`) must still omit `Timezone`. The canonical UTC
        // ids carry no offset/DST, so emitting a zeroed TZI blob would be
        // redundant (and several EAS clients reject it). Guards the
        // `is_utc_zone` gate added for C6.
        for utc_id in ["UTC", "Etc/UTC", "GMT"] {
            let item = CalendarItem {
                uid: format!("utc-002-{utc_id}"),
                subject: "UTC Meeting".to_string(),
                start: chrono::Utc.with_ymd_and_hms(2025, 1, 1, 9, 0, 0).unwrap(),
                end: chrono::Utc.with_ymd_and_hms(2025, 1, 1, 10, 0, 0).unwrap(),
                all_day: false,
                timezone: Some(utc_id.to_string()),
                ..Default::default()
            };
            let options = crate::eas_sync_options::EasSyncCollectionOptions::default();
            let xml = render_calendar_app_data_with_options(&item, &options);
            assert!(
                !xml.contains("<Calendar:Timezone>"),
                "explicit UTC ({utc_id}) event must omit <Calendar:Timezone>:\n{xml}"
            );
        }
    }

    // ---- Calendar delta-path end-to-end tests (AUDIT3 findings #5/#6/#7) ----

    use serde_json::{Value, json};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// Shared state for the scripted calendar JMAP stub.
    #[derive(Clone)]
    struct CalStub {
        base: String,
        /// Scripted `CalendarEvent/changes` response argument objects, served
        /// in order; the last one repeats if a call exceeds the script.
        changes_pages: std::sync::Arc<Mutex<Vec<Value>>>,
        change_idx: std::sync::Arc<AtomicUsize>,
        /// Full event objects returned by `CalendarEvent/get`.
        events: std::sync::Arc<Mutex<Vec<Value>>>,
        query_state: std::sync::Arc<String>,
        get_state: std::sync::Arc<String>,
        /// Every parsed request body, for assertions.
        recorded: std::sync::Arc<Mutex<Vec<Value>>>,
        /// When set, `CalendarEvent/changes` answers with this method error.
        changes_error: std::sync::Arc<Option<String>>,
        /// Reported `CalendarEvent/query` total; 0 means "use the event count".
        query_total: std::sync::Arc<AtomicUsize>,
        /// When true, `CalendarEvent/query` honours `position`/`limit` like the
        /// real backend (paging a sorted id list), instead of returning the
        /// whole list on every call.
        honour_position: std::sync::Arc<AtomicBool>,
        /// Calendar objects returned by `Calendar/get`, used by the folder
        /// scoping path (`resolve_calendar_id_for_href`).
        calendars: std::sync::Arc<Mutex<Vec<Value>>>,
        /// CalDAV surface calls (method, path) recorded by the fallback route.
        dav_calls: std::sync::Arc<Mutex<Vec<(String, String)>>>,
    }

    impl CalStub {
        fn with_calendars(self, calendars: Vec<Value>) -> Self {
            // Mutate the shared `Calendar/get` list in place: the router holds a
            // clone of this stub, so replacing the `Arc` would not reach it.
            *self.calendars.lock().unwrap() = calendars;
            self
        }

        /// Replace the scripted `CalendarEvent/changes` pages and reset the page
        /// cursor, so a second `Sync` in the same test can script a new delta.
        fn set_changes(&self, pages: Vec<Value>) {
            *self.changes_pages.lock().unwrap() = pages;
            self.change_idx
                .store(0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    async fn spawn_calendar_stub(
        changes_pages: Vec<Value>,
        events: Vec<Value>,
        query_state: &str,
        get_state: &str,
        changes_error: Option<String>,
    ) -> CalStub {
        use axum::response::Response;
        use axum::routing::{any, get, post};

        async fn session(axum::Extension(s): axum::Extension<CalStub>) -> axum::Json<Value> {
            axum::Json(json!({
                "apiUrl": s.base,
                "downloadUrl": format!("{}/download/{{accountId}}/{{blobId}}", s.base),
                "uploadUrl": format!("{}/upload/{{accountId}}", s.base),
                "eventSourceUrl": format!("{}/eventsource", s.base),
                "state": "s",
                "username": "u",
                "accounts": {
                    "acct": { "name": "u", "isPersonal": true, "isReadOnly": false, "accountCapabilities": {} }
                },
                "primaryAccounts": { crate::jmap::JMAP_CAL_CAPABILITY: "acct" },
                "capabilities": {
                    crate::jmap::JMAP_CORE_CAPABILITY: { "maxObjectsInGet": 500 },
                    crate::jmap::JMAP_CAL_CAPABILITY: {},
                },
            }))
        }

        async fn api(
            axum::Extension(s): axum::Extension<CalStub>,
            axum::Json(body): axum::Json<Value>,
        ) -> axum::Json<Value> {
            s.recorded.lock().unwrap().push(body.clone());
            let calls = body
                .get("methodCalls")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            let mut responses = Vec::new();
            for call in calls {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                let args = call.get(1).cloned().unwrap_or(Value::Null);
                let id = call.get(2).and_then(|v| v.as_str()).unwrap_or("x");
                match name {
                    "CalendarEvent/changes" => {
                        if let Some(err) = s.changes_error.as_ref() {
                            responses.push(json!([
                                "error",
                                { "type": err, "description": "scripted" },
                                id
                            ]));
                            continue;
                        }
                        let idx = s.change_idx.fetch_add(1, Ordering::SeqCst);
                        let pages = s.changes_pages.lock().unwrap();
                        let page = pages
                            .get(idx)
                            .or_else(|| pages.last())
                            .cloned()
                            .unwrap_or_else(|| json!({}));
                        responses.push(json!(["CalendarEvent/changes", page, id]));
                    }
                    "CalendarEvent/query" => {
                        // Honour `filter.inCalendarIds` (draft-ietf-jmap-
                        // calendars §5.11.1) so a scoped query sees only its
                        // own calendar's events, matching a real backend.
                        let want_cal: Vec<String> = args
                            .get("filter")
                            .and_then(|f| f.get("inCalendarIds"))
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(String::from)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let mut ids: Vec<String> = s
                            .events
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|e| {
                                want_cal.is_empty()
                                    || e.get("calendarIds")
                                        .and_then(|v| v.as_object())
                                        .map(|m| want_cal.iter().any(|c| m.contains_key(c)))
                                        .unwrap_or(false)
                            })
                            .filter_map(|e| e.get("id").and_then(|v| v.as_str()).map(String::from))
                            .collect();
                        ids.sort();
                        let total = s.query_total.load(Ordering::SeqCst);
                        let total = if total == 0 { ids.len() } else { total };
                        let page_ids = if s.honour_position.load(Ordering::SeqCst) {
                            let position =
                                args.get("position").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let limit =
                                args.get("limit").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            let end = position.saturating_add(limit).min(ids.len());
                            let start = position.min(ids.len());
                            ids[start..end].to_vec()
                        } else {
                            ids
                        };
                        responses.push(json!([
                            "CalendarEvent/query",
                            { "ids": page_ids, "total": total, "queryState": s.query_state.as_str() },
                            id
                        ]));
                    }
                    "CalendarEvent/get" => {
                        let want: Vec<String> = args
                            .get("ids")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str())
                                    .map(String::from)
                                    .collect()
                            })
                            .unwrap_or_default();
                        let list: Vec<Value> = s
                            .events
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|e| {
                                want.is_empty()
                                    || e.get("id")
                                        .and_then(|v| v.as_str())
                                        .map(|i| want.iter().any(|w| w == i))
                                        .unwrap_or(false)
                            })
                            .cloned()
                            .collect();
                        responses.push(json!([
                            "CalendarEvent/get",
                            { "accountId": "acct", "state": s.get_state.as_str(), "list": list, "notFound": [] },
                            id
                        ]));
                    }
                    "Calendar/get" => {
                        let list = s.calendars.lock().unwrap().clone();
                        responses.push(json!([
                            "Calendar/get",
                            { "accountId": "acct", "state": "cs", "list": list },
                            id
                        ]));
                    }
                    _ => {}
                }
            }
            axum::Json(json!({ "methodResponses": responses }))
        }

        /// CalDAV fallback surface: records PUT/PROPFIND and answers PROPFIND
        /// with the collections `find_user_calendars` expects.
        async fn dav(
            axum::Extension(s): axum::Extension<CalStub>,
            method: axum::http::Method,
            uri: axum::http::Uri,
            body: String,
        ) -> Response {
            s.dav_calls
                .lock()
                .unwrap()
                .push((method.as_str().to_string(), uri.path().to_string()));
            if method.as_str() == "PROPFIND" {
                let names: Vec<String> = s
                    .calendars
                    .lock()
                    .unwrap()
                    .iter()
                    .filter_map(|c| c.get("id").and_then(|v| v.as_str()).map(String::from))
                    .collect();
                let mut responses = String::new();
                for name in names {
                    responses.push_str(&format!(
                        "<D:response><D:href>{}/{}/</D:href><D:propstat><D:prop>\
<D:resourcetype><D:collection/><C:calendar/></D:resourcetype>\
<D:displayname>{name}</D:displayname></D:prop><D:status>HTTP/1.1 200 OK</D:status>\
</D:propstat></D:response>",
                        uri.path().trim_end_matches('/'),
                        name
                    ));
                }
                let body = format!(
                    "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
<D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">{responses}</D:multistatus>"
                );
                return Response::builder()
                    .status(207)
                    .header("content-type", "application/xml; charset=utf-8")
                    .body(axum::body::Body::from(body))
                    .unwrap();
            }
            if method.as_str() == "PUT" {
                return Response::builder()
                    .status(201)
                    .header("etag", "\"dav-etag\"")
                    .body(axum::body::Body::from(body))
                    .unwrap();
            }
            Response::builder()
                .status(200)
                .body(axum::body::Body::empty())
                .unwrap()
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let stub = CalStub {
            base: base.clone(),
            changes_pages: std::sync::Arc::new(Mutex::new(changes_pages)),
            change_idx: std::sync::Arc::new(AtomicUsize::new(0)),
            events: std::sync::Arc::new(Mutex::new(events)),
            query_state: std::sync::Arc::new(query_state.to_string()),
            get_state: std::sync::Arc::new(get_state.to_string()),
            recorded: std::sync::Arc::new(Mutex::new(Vec::new())),
            changes_error: std::sync::Arc::new(changes_error),
            query_total: std::sync::Arc::new(AtomicUsize::new(0)),
            honour_position: std::sync::Arc::new(AtomicBool::new(false)),
            calendars: std::sync::Arc::new(Mutex::new(Vec::new())),
            dav_calls: std::sync::Arc::new(Mutex::new(Vec::new())),
        };
        let app = axum::Router::new()
            .route("/session", get(session))
            .route("/", post(api))
            .fallback(any(dav))
            .layer(axum::Extension(stub.clone()));
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        stub
    }

    fn stub_event(id: &str, uid: &str, title: &str) -> Value {
        json!({
            "id": id,
            "uid": uid,
            "title": title,
            "start": "2026-01-05T09:00:00",
            "timeZone": "Europe/Berlin",
            "duration": "PT1H",
            "iCalendar": {
                "convertedProperties": { "duration": { "name": "dtend" } },
                "name": "vevent"
            }
        })
    }

    /// A stub event carrying an explicit `calendarIds` membership, so the
    /// scoped prime/delta paths can filter by calendar.
    fn scoped_event(id: &str, uid: &str, title: &str, calendar_id: &str) -> Value {
        let mut event = stub_event(id, uid, title);
        event["calendarIds"] = json!({ calendar_id: true });
        event
    }

    async fn calendar_sync_state(jmap_base: &str) -> Arc<AppState> {
        use crate::models::AppState;
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: jmap_base.to_string(),
            email_enabled: true,
            prefer_jmap_calendar: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(AppState::new(cfg, Arc::new(storage)))
    }

    /// State whose CalDAV base points at the same stub, for the client-mutation
    /// path (which writes through CalDAV).
    async fn calendar_mutation_state(base: &str) -> Arc<AppState> {
        use crate::models::AppState;
        let storage = crate::storage::Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        let cfg = crate::config::Config {
            jmap_base: base.to_string(),
            caldav_base: base.to_string(),
            email_enabled: true,
            prefer_jmap_calendar: true,
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            ..Default::default()
        };
        Arc::new(AppState::new(cfg, Arc::new(storage)))
    }

    async fn run_calendar_sync(state: &Arc<AppState>, incoming_sync_key: &str) -> String {
        run_calendar_sync_for(state, "calendar", "calendar", incoming_sync_key).await
    }

    /// Drive a calendar `Sync` for an explicit `collection_id` /
    /// `state_collection_id` (a custom calendar-family folder uses its CalDAV
    /// href as the CollectionId and a device-scoped state key).
    async fn run_calendar_sync_for(
        state: &Arc<AppState>,
        collection_id: &str,
        state_collection_id: &str,
        incoming_sync_key: &str,
    ) -> String {
        let opts = SyncOptions {
            window_size: 100,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        };
        perform_sync(&PerformSyncParams {
            state: state.clone(),
            owner: "user@example.com",
            collection_id,
            state_collection_id,
            incoming_sync_key,
            content_class: "Calendar",
            opts,
            username: "user@example.com",
            password: "pw",
            client_mutation_responses: "",
        })
        .await
        .expect("perform_sync")
    }

    /// Finding #5: a `CalendarEvent/changes` chain that reports
    /// `hasMoreChanges` must be drained to the FINAL `newState`, and every
    /// page's events delivered. Storing an intermediate state would make the
    /// next delta skip the undrained pages.
    #[tokio::test]
    async fn calendar_delta_drains_all_pages_and_stores_final_state() {
        let page1 = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": true,
            "created": ["e1"], "updated": [], "destroyed": []
        });
        let page2 = json!({
            "accountId": "acct", "oldState": "s1", "newState": "s2",
            "hasMoreChanges": false,
            "created": ["e2"], "updated": [], "destroyed": []
        });
        let stub = spawn_calendar_stub(
            vec![page1, page2],
            vec![
                stub_event("e1", "uid-e1", "Event One"),
                stub_event("e2", "uid-e2", "Event Two"),
            ],
            "query-s",
            "get-s",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        let xml = run_calendar_sync(&state, "key-1").await;
        assert!(xml.contains("uid-e1"), "page 1 event delivered:\n{xml}");
        assert!(xml.contains("uid-e2"), "page 2 event delivered:\n{xml}");

        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            token.as_deref(),
            Some("s2"),
            "final state, not an intermediate"
        );

        // The delta must have walked two `CalendarEvent/changes` pages.
        let rec = stub.recorded.lock().unwrap().clone();
        let changes_calls: Vec<_> = rec
            .iter()
            .filter(|b| b.to_string().contains("CalendarEvent/changes"))
            .collect();
        assert_eq!(changes_calls.len(), 2, "both pages requested");
    }

    /// Finding #7: the initial prime stores the `CalendarEvent/get` data-type
    /// `state` — the token `CalendarEvent/changes` accepts — not the query's
    /// `queryState`.
    #[tokio::test]
    async fn calendar_prime_stores_get_state_not_query_state() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![stub_event("e1", "uid-e1", "Event One")],
            "query-7",
            "get-9",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;

        let xml = run_calendar_sync(&state, "0").await;
        assert!(xml.contains("uid-e1"), "prime delivers the event:\n{xml}");

        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            token.as_deref(),
            Some("get-9"),
            "the CalendarEvent/get state, not queryState"
        );
    }

    /// Finding #7: a `cannotCalculateChanges` reply re-primes from a full
    /// query (instead of falling back to CalDAV) and stores the fresh state.
    #[tokio::test]
    async fn calendar_cannot_calculate_changes_reprimes() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![stub_event("e1", "uid-e1", "Event One")],
            "query-7",
            "get-9",
            Some("cannotCalculateChanges".to_string()),
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("stale"))
            .await
            .unwrap();

        let xml = run_calendar_sync(&state, "key-1").await;
        assert!(
            xml.contains("uid-e1"),
            "re-prime must deliver the event:\n{xml}"
        );
        assert!(
            xml.contains("<Status>1</Status>"),
            "re-prime must succeed, not fall back to CalDAV:\n{xml}"
        );

        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(token.as_deref(), Some("get-9"), "fresh state stored");
    }

    /// Finding #6: the same event object yields the same content-hash etag on
    /// the initial prime and a later delta, so etag-based change detection is
    /// stable across both paths.
    #[tokio::test]
    async fn calendar_etag_is_stable_across_prime_and_delta() {
        let event = stub_event("e1", "uid-e1", "Event One");
        let expected = crate::jmap::calendar_event_etag(&event, None);

        // Prime.
        let prime = spawn_calendar_stub(vec![], vec![event.clone()], "q", "get-s", None).await;
        let prime_state = calendar_sync_state(&prime.base).await;
        run_calendar_sync(&prime_state, "0").await;
        let stored_prime = prime_state
            .storage
            .list_ews_items("user@example.com", 16, 0)
            .await
            .unwrap();
        assert_eq!(stored_prime.len(), 1);
        assert_eq!(stored_prime[0].etag.as_deref(), Some(expected.as_str()));

        // Delta.
        let page = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": false,
            "created": [], "updated": ["e1"], "destroyed": []
        });
        let delta = spawn_calendar_stub(vec![page], vec![event], "q", "get-s", None).await;
        let delta_state = calendar_sync_state(&delta.base).await;
        delta_state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("s0"))
            .await
            .unwrap();
        run_calendar_sync(&delta_state, "key-1").await;
        let stored_delta = delta_state
            .storage
            .list_ews_items("user@example.com", 16, 0)
            .await
            .unwrap();
        assert_eq!(stored_delta.len(), 1);
        assert_eq!(
            stored_delta[0].etag.as_deref(),
            Some(expected.as_str()),
            "the delta etag must match the prime etag for unchanged content"
        );
    }

    /// A clipped delta window (MoreAvailable) must NOT advance the stored
    /// token: the undelivered changes are still behind the old token, and the
    /// next Sync re-derives them (the already-delivered ones are filtered by
    /// the `item_map` etag comparison).
    #[tokio::test]
    async fn calendar_clipped_window_preserves_token() {
        let page = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": false,
            "created": ["e1", "e2"], "updated": [], "destroyed": []
        });
        let stub = spawn_calendar_stub(
            vec![page],
            vec![
                stub_event("e1", "uid-e1", "Event One"),
                stub_event("e2", "uid-e2", "Event Two"),
            ],
            "q",
            "get-s",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        // Window of 1: only one of the two changed events fits.
        let opts = SyncOptions {
            window_size: 1,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        };
        let xml = perform_sync(&PerformSyncParams {
            state: state.clone(),
            owner: "user@example.com",
            collection_id: "calendar",
            state_collection_id: "calendar",
            incoming_sync_key: "key-1",
            content_class: "Calendar",
            opts,
            username: "user@example.com",
            password: "pw",
            client_mutation_responses: "",
        })
        .await
        .expect("perform_sync");

        assert!(
            xml.contains("<MoreAvailable/>"),
            "a clipped window must signal MoreAvailable:\n{xml}"
        );
        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            token.as_deref(),
            Some("s0"),
            "a clipped window must keep the old token so the next delta re-derives the set"
        );
    }

    /// A prime whose query reports more events than it returned (a truncated
    /// snapshot) must NOT tombstone stored events that are absent from it.
    #[tokio::test]
    async fn calendar_truncated_prime_does_not_tombstone() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![stub_event("e2", "uid-e2", "Event Two")],
            "q",
            "get-s",
            None,
        )
        .await;
        // The server says there are more events than the page carries.
        stub.query_total.store(50, Ordering::SeqCst);
        let state = calendar_sync_state(&stub.base).await;
        // A stored event absent from the (truncated) page.
        state
            .storage
            .upsert_item_map(
                "user@example.com",
                "jmap://calendar/acct/e1",
                "jmap://calendar/acct/e1",
                "sid-e1",
                "uid-e1",
                "old-etag",
            )
            .await
            .unwrap();

        run_calendar_sync(&state, "0").await;

        let stored = state
            .storage
            .list_ews_items("user@example.com", 16, 0)
            .await
            .unwrap();
        let ids: Vec<&str> = stored.iter().map(|r| r.server_id.as_str()).collect();
        assert!(
            ids.contains(&"sid-e1"),
            "a truncated prime must not delete the event it did not return: {ids:?}"
        );
    }

    /// `query_all_calendar_events` must page `CalendarEvent/query` by
    /// `position` until every id in `total` has been seen, so a backend that
    /// caps query page size never silently truncates the prime snapshot.
    #[tokio::test]
    async fn calendar_prime_pages_query_by_position() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![
                stub_event("e1", "uid-e1", "Event One"),
                stub_event("e2", "uid-e2", "Event Two"),
                stub_event("e3", "uid-e3", "Event Three"),
                stub_event("e4", "uid-e4", "Event Four"),
            ],
            "query-7",
            "get-9",
            None,
        )
        .await;
        stub.honour_position.store(true, Ordering::SeqCst);
        let state = calendar_sync_state(&stub.base).await;
        let xml = run_calendar_sync(&state, "0").await;

        let adds = xml.matches("<Add><ServerId>").count();
        assert_eq!(
            adds, 4,
            "all 4 events must be delivered across query pages:\n{xml}"
        );
        let recorded = stub.recorded.lock().unwrap().clone();
        let mut positions = Vec::new();
        for body in &recorded {
            let calls = body
                .get("methodCalls")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();
            for call in calls.iter() {
                let name = call.get(0).and_then(|v| v.as_str()).unwrap_or("");
                if name == "CalendarEvent/query"
                    && let Some(position) = call
                        .get(1)
                        .and_then(|a| a.get("position"))
                        .and_then(|v| v.as_u64())
                {
                    positions.push(position);
                }
            }
        }
        assert!(
            !positions.is_empty(),
            "the client must page the query by position: {recorded:?}"
        );
        assert_eq!(
            positions[0], 0,
            "the first query page starts at position 0: {recorded:?}"
        );
    }

    /// A `CalendarEvent/changes` drain that stops at its cap with
    /// `hasMoreChanges` set must signal `<MoreAvailable/>` even when the
    /// accumulated changes fit the EAS window, and the INTERMEDIATE state
    /// must be stored so the next delta resumes from `newState` rather
    /// than re-delivering the same pages forever.
    #[tokio::test]
    async fn calendar_drain_cap_signals_more_available_and_keeps_intermediate_state() {
        let page1 = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": true,
            "created": ["e1"], "updated": [], "destroyed": []
        });
        let page2 = json!({
            "accountId": "acct", "oldState": "s1", "newState": "s2",
            "hasMoreChanges": true,
            "created": ["e2"], "updated": [], "destroyed": []
        });
        let stub = spawn_calendar_stub(
            vec![page1, page2],
            vec![
                stub_event("e1", "uid-e1", "Event One"),
                stub_event("e2", "uid-e2", "Event Two"),
            ],
            "query-7",
            "get-9",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("s0"))
            .await
            .unwrap();
        let xml = run_calendar_sync(&state, "key-1").await;

        assert!(
            xml.contains("<MoreAvailable/>"),
            "a capped drain must signal MoreAvailable:\n{xml}"
        );
        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            token.as_deref(),
            Some("s2"),
            "the intermediate newState from the capped drain must be persisted: {token:?}"
        );
    }

    /// A clipped PRIME window (MoreAvailable) must store a **per-prime**
    /// cursor (`prime-cursor:<delivered-href>`) instead of a provider token,
    /// and the follow-up Sync must make forward progress: it re-primes from
    /// the cursor and force-delivers the remainder even when the shared
    /// `item_map` already holds a matching etag (another device's completed
    /// prime may have populated the map with events this device never saw).
    /// Only the fully-delivered window stores the provider get-state token.
    #[tokio::test]
    async fn calendar_clipped_prime_reprimes_and_progresses() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![
                stub_event("e1", "uid-e1", "Event One"),
                stub_event("e2", "uid-e2", "Event Two"),
            ],
            "query-7",
            "get-9",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;

        let make_opts = || SyncOptions {
            window_size: 1,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        };

        // First Sync: window clips after one event.
        let first = perform_sync(&PerformSyncParams {
            state: state.clone(),
            owner: "user@example.com",
            collection_id: "calendar",
            state_collection_id: "calendar",
            incoming_sync_key: "0",
            content_class: "Calendar",
            opts: make_opts(),
            username: "user@example.com",
            password: "pw",
            client_mutation_responses: "",
        })
        .await
        .expect("perform_sync");
        assert!(
            first.contains("<MoreAvailable/>"),
            "a clipped prime must signal MoreAvailable:\n{first}"
        );
        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert!(
            token
                .as_deref()
                .is_some_and(|t| t.starts_with("prime-cursor:")),
            "a clipped prime must store a prime cursor, not a provider token: {token:?}"
        );
        let first_key = first
            .split("<SyncKey>")
            .nth(1)
            .and_then(|s| s.split("</SyncKey>").next())
            .expect("SyncKey in response")
            .to_string();
        assert_eq!(
            state
                .storage
                .list_ews_items("user@example.com", 16, 0)
                .await
                .unwrap()
                .len(),
            1,
            "only the delivered window is stored"
        );

        // Simulate a second device completing its own prime: delete this
        // device's item_map row so a re-prime that relied on etag filtering
        // would refill from empty — but the cursor continuation must
        // force-deliver the remainder regardless of item_map state.
        let stored_before = state
            .storage
            .list_ews_items("user@example.com", 16, 0)
            .await
            .unwrap();
        let stored_server_id = stored_before[0].server_id.clone();
        state
            .storage
            .delete_item_by_server_id("user@example.com", &stored_server_id)
            .await
            .unwrap();
        assert_eq!(
            state
                .storage
                .list_ews_items("user@example.com", 16, 0)
                .await
                .unwrap()
                .len(),
            0,
            "item_map wiped to force the etag-filter would have nothing to match"
        );

        // Follow-up Sync resumes the prime from the cursor and delivers the
        // remaining event.
        let second = perform_sync(&PerformSyncParams {
            state: state.clone(),
            owner: "user@example.com",
            collection_id: "calendar",
            state_collection_id: "calendar",
            incoming_sync_key: &first_key,
            content_class: "Calendar",
            opts: make_opts(),
            username: "user@example.com",
            password: "pw",
            client_mutation_responses: "",
        })
        .await
        .expect("perform_sync");
        assert!(
            !second.contains("<MoreAvailable/>"),
            "the second window completes the set:\n{second}"
        );
        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            token.as_deref(),
            Some("get-9"),
            "the completed prime stores the get state"
        );
        assert_eq!(
            state
                .storage
                .list_ews_items("user@example.com", 16, 0)
                .await
                .unwrap()
                .len(),
            1,
            "only the forced remainder event is stored after the cursor continuation"
        );
    }

    /// An incomplete prime snapshot must store NO token, so the next Sync
    /// re-primes from the full snapshot rather than attempting a delta.
    #[tokio::test]
    async fn calendar_incomplete_prime_stores_no_token() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![stub_event("e2", "uid-e2", "Event Two")],
            "q",
            "get-s",
            None,
        )
        .await;
        stub.query_total.store(50, Ordering::SeqCst);
        let state = calendar_sync_state(&stub.base).await;

        let xml = run_calendar_sync(&state, "0").await;
        assert!(
            !xml.contains("<MoreAvailable/>"),
            "an unclipped incomplete prime must not signal MoreAvailable:\n{xml}"
        );
        let (_, token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert!(
            token.is_none(),
            "an incomplete snapshot must store no delta token: {token:?}"
        );
    }

    /// A destroyed item whose `<Delete>` was clipped away must be RETAINED
    /// in `item_map` so the held-token retry re-queues its delete, and the
    /// row must be removed only once the `<Delete>` is actually emitted.
    #[tokio::test]
    async fn calendar_clipped_destroy_retains_row_until_delete_emitted() {
        let page = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": false,
            "created": ["e1"], "updated": [], "destroyed": ["e9"]
        });
        let stub = spawn_calendar_stub(
            vec![page],
            vec![stub_event("e1", "uid-e1", "Event One")],
            "q",
            "get-s",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        let href_e9 = "jmap://calendar/acct/e9";
        let sid_e9 = generate_server_id(state.cfg.hmac_secret(), href_e9);
        state
            .storage
            .upsert_item_map(
                "user@example.com",
                href_e9,
                href_e9,
                &sid_e9,
                "uid-e9",
                "old-etag",
            )
            .await
            .unwrap();
        state
            .storage
            .set_sync_key("user@example.com", "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        let make_opts = || SyncOptions {
            window_size: 1,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        };
        let run = |key: String| {
            let state = state.clone();
            async move {
                perform_sync(&PerformSyncParams {
                    state,
                    owner: "user@example.com",
                    collection_id: "calendar",
                    state_collection_id: "calendar",
                    incoming_sync_key: &key,
                    content_class: "Calendar",
                    opts: make_opts(),
                    username: "user@example.com",
                    password: "pw",
                    client_mutation_responses: "",
                })
                .await
                .expect("perform_sync")
            }
        };

        // Window of 1: the Add fills the window, so the Delete is clipped.
        let first = run("key-1".to_string()).await;
        assert!(
            first.contains("<MoreAvailable/>"),
            "the clipped delete must signal MoreAvailable:\n{first}"
        );
        assert!(
            !first.contains("<Delete>"),
            "the delete must not be emitted in the clipped window:\n{first}"
        );
        assert!(
            state
                .storage
                .list_ews_items("user@example.com", 16, 0)
                .await
                .unwrap()
                .iter()
                .any(|r| r.server_id == sid_e9),
            "the destroyed row must be RETAINED until its Delete is emitted"
        );
        let (_, first_token) = state
            .storage
            .get_sync_key("user@example.com", "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_eq!(
            first_token.as_deref(),
            Some("s0"),
            "a clipped delta must keep the old token"
        );
        let first_key = first
            .split("<SyncKey>")
            .nth(1)
            .and_then(|s| s.split("</SyncKey>").next())
            .expect("SyncKey in response")
            .to_string();

        // Retry with the held token: the retained row is re-queued and the
        // Delete is now emitted (e1 is unchanged, so it is filtered out).
        let second = run(first_key).await;
        assert!(
            second.contains("<Delete>"),
            "the retry must emit the Delete:\n{second}"
        );
        assert!(
            second.contains(&sid_e9),
            "the emitted Delete must name the destroyed event:\n{second}"
        );
        assert!(
            !state
                .storage
                .list_ews_items("user@example.com", 16, 0)
                .await
                .unwrap()
                .iter()
                .any(|r| r.server_id == sid_e9),
            "the row must be removed after its Delete is emitted"
        );
    }

    /// A client Add into a user-created calendar-family folder must land in
    /// THAT folder's CalDAV collection, resolved from the folder's stored
    /// href. The mutation path receives the device-scoped CollectionId
    /// (`{visible}::{device}`), so the href lookup must strip that suffix.
    #[tokio::test]
    async fn calendar_add_scopes_put_to_the_folders_own_collection() {
        let stub = spawn_calendar_stub(vec![], vec![], "query-s", "get-s", None)
            .await
            .with_calendars(vec![json!({
                "id": "cal-side",
                "name": "Side Project",
                "href": "/dav/cal/user@example.com/Side%20Project/"
            })]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner,
                    server_id: folder_href,
                    parent_id: "0",
                    display_name: "Side Project",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: folder_href,
                },
                "folder-add",
            )
            .await
            .unwrap();

        let xml = "<Sync><Collections><Collection><Class>Calendar</Class>\
<CollectionId>/dav/cal/user@example.com/Side%20Project/</CollectionId>\
<Add><ClientId>c1</ClientId><ApplicationData>\
<Calendar:Subject>Scoped</Calendar:Subject>\
<Calendar:StartTime>2026-01-05T09:00:00Z</Calendar:StartTime>\
<Calendar:EndTime>2026-01-05T10:00:00Z</Calendar:EndTime>\
</ApplicationData></Add></Collection></Collections></Sync>";
        let results = apply_client_sync_mutations(
            state.clone(),
            owner,
            &crate::eas::scoped_collection_id(folder_href, "dev1"),
            owner,
            "pw",
            xml,
        )
        .await
        .expect("mutations apply");
        assert!(
            matches!(
                results.as_slice(),
                [ClientMutationResult::Add { status: "1", .. }]
            ),
            "the Add must succeed: {results:?}"
        );
        let puts: Vec<String> = stub
            .dav_calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(m, _)| m == "PUT")
            .map(|(_, p)| p.clone())
            .collect();
        assert!(
            puts.iter().any(|p| p.contains("Side%20Project")),
            "the PUT must target the folder's own collection, got {puts:?}"
        );
    }
    /// Regression test for the fail-closed `?` on the folder lookup in
    /// `apply_client_sync_mutations`: when the `folder_map` read itself fails,
    /// the mutation must NOT fall back to the default calendar — an Add would
    /// otherwise be written to a collection the folder does not own. The
    /// lookup error propagates so the caller answers a retryable Status 6 and
    /// no PUT reaches the CalDAV stub.
    #[tokio::test]
    async fn calendar_add_fails_closed_when_folder_lookup_fails() {
        let stub = spawn_calendar_stub(vec![], vec![], "query-s", "get-s", None)
            .await
            .with_calendars(vec![json!({
                "id": "cal-default",
                "name": "Calendar",
                "href": "/dav/cal/user@example.com/Calendar/"
            })]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";

        // Poison the folder_map read so `collection_folder_href` errors.
        sqlx::query("DROP TABLE folder_map")
            .execute(state.storage.pool())
            .await
            .unwrap();

        let xml = "<Sync><Collections><Collection><Class>Calendar</Class>\
<CollectionId>/dav/cal/user@example.com/Side%20Project/</CollectionId>\
<Add><ClientId>c1</ClientId><ApplicationData>\
<Calendar:Subject>Scoped</Calendar:Subject>\
<Calendar:StartTime>2026-01-05T09:00:00Z</Calendar:StartTime>\
<Calendar:EndTime>2026-01-05T10:00:00Z</Calendar:EndTime>\
</ApplicationData></Add></Collection></Collections></Sync>";
        let result = apply_client_sync_mutations(
            state.clone(),
            owner,
            &crate::eas::scoped_collection_id(folder_href, "dev1"),
            owner,
            "pw",
            xml,
        )
        .await;
        assert!(
            result.is_err(),
            "a folder-lookup failure must fail the mutation closed: {result:?}"
        );
        assert!(
            stub.dav_calls
                .lock()
                .unwrap()
                .iter()
                .all(|(m, _)| m != "PUT"),
            "no Add may reach a calendar when the folder cannot be resolved"
        );
    }

    /// A user-created calendar-family folder must sync ONLY its own calendar:
    /// the prime resolves the folder's CalDAV href to its JMAP calendar id and
    /// scopes the query to it, so events from other calendars never reach the
    /// folder.
    #[tokio::test]
    async fn calendar_scoped_folder_prime_only_delivers_its_own_calendar() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![
                scoped_event("e-mine", "uid-mine", "Mine", "cal-side"),
                scoped_event("e-other", "uid-other", "Other", "cal-default"),
            ],
            "query-s",
            "get-s",
            None,
        )
        .await
        .with_calendars(vec![json!({
            "id": "cal-side",
            "name": "Side Project",
            "href": "/dav/cal/user@example.com/Side%20Project/"
        })]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner,
                    server_id: folder_href,
                    parent_id: "0",
                    display_name: "Side Project",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: folder_href,
                },
                "folder-add",
            )
            .await
            .unwrap();

        let xml =
            run_calendar_sync_for(&state, folder_href, &format!("{folder_href}::dev1"), "0").await;
        assert!(
            xml.contains("uid-mine"),
            "the folder's own event must be delivered:\n{xml}"
        );

        // The scoped prime must issue its `CalendarEvent/query` with
        // `filter.inCalendarIds` naming ONLY the folder's own calendar, so a
        // real backend never returns another calendar's events. (The stub does
        // not resolve the `#ids` back-reference, so the wire query is the
        // assertion target.)
        let scoped = stub.recorded.lock().unwrap().iter().any(|body| {
            body.get("methodCalls")
                .and_then(|c| c.as_array())
                .map(|calls| {
                    calls.iter().any(|c| {
                        c.get(0).and_then(|v| v.as_str()) == Some("CalendarEvent/query")
                            && c.get(1)
                                .and_then(|a| a.get("filter"))
                                .and_then(|f| f.get("inCalendarIds"))
                                .and_then(|v| v.as_array())
                                .map(|ids| ids.iter().any(|i| i.as_str() == Some("cal-side")))
                                .unwrap_or(false)
                    })
                })
                .unwrap_or(false)
        });
        assert!(
            scoped,
            "the folder's prime must scope the query to its own calendar id"
        );
    }

    /// Finding E: `CalendarEvent/changes` is ACCOUNT-wide, so a destroyed id in
    /// a scoped (custom-calendar) folder may belong to a DIFFERENT calendar.
    /// Because `item_map` is owner-global (no collection column), tombstoning
    /// such an id would emit a `<Delete>` in this folder for an event the
    /// client never held AND remove the shared row before the event's own
    /// collection could deliver its Delete. A scoped folder must not infer
    /// deletions from the account feed.
    #[tokio::test]
    async fn scoped_calendar_delta_does_not_tombstone_foreign_destroyed_event() {
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";
        let stub = spawn_calendar_stub(
            vec![json!({
                "accountId": "acct", "oldState": "s0", "newState": "s1",
                "hasMoreChanges": false,
                "created": [], "updated": [], "destroyed": ["e-other"]
            })],
            // The destroyed event is still fetchable in this stub (the destroy
            // and the get race on a real backend); the folder must not care.
            vec![scoped_event("e-other", "uid-other", "Other", "cal-default")],
            "query-s",
            "get-s",
            None,
        )
        .await
        .with_calendars(vec![json!({
            "id": "cal-side",
            "name": "Side Project",
            "href": folder_href
        })]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner,
                    server_id: folder_href,
                    parent_id: "0",
                    display_name: "Side Project",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: folder_href,
                },
                "folder-add",
            )
            .await
            .unwrap();

        // A stored event that lives in ANOTHER calendar, mirrored in the shared
        // item_map.
        let foreign_href = "jmap://calendar/acct/e-other";
        let foreign_server_id = generate_server_id(state.cfg.hmac_secret(), foreign_href);
        state
            .storage
            .upsert_item_map(
                owner,
                foreign_href,
                foreign_href,
                &foreign_server_id,
                "uid-other",
                "etag",
            )
            .await
            .unwrap();

        let state_collection_id = format!("{folder_href}::dev1");
        state
            .storage
            .set_sync_key(owner, &state_collection_id, "key-1", Some("s0"))
            .await
            .unwrap();

        let xml = run_calendar_sync_for(&state, folder_href, &state_collection_id, "key-1").await;
        assert!(
            !xml.contains("<Delete>"),
            "a scoped folder must not delete an event from another calendar:\n{xml}"
        );
        assert!(
            state
                .storage
                .get_ews_item_by_server_id(owner, &foreign_server_id)
                .await
                .unwrap()
                .is_some(),
            "the shared item_map row of the foreign calendar's event must survive"
        );
        assert!(
            state
                .storage
                .list_deleted_since(owner, 0)
                .await
                .unwrap()
                .is_empty(),
            "no tombstone may be recorded for a foreign calendar's destroyed id"
        );
    }

    /// A scoped folder's PRIME is a snapshot of ONE calendar, not the account.
    /// The full-snapshot tombstone sweep must not treat another calendar's
    /// stored event (owner-global `item_map`) as deleted just because this
    /// calendar's snapshot omits it.
    #[tokio::test]
    async fn scoped_calendar_prime_does_not_tombstone_foreign_event() {
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";
        let stub = spawn_calendar_stub(
            vec![],
            vec![scoped_event("e-mine", "uid-mine", "Mine", "cal-side")],
            "query-s",
            "get-s",
            None,
        )
        .await
        .with_calendars(vec![json!({
            "id": "cal-side",
            "name": "Side Project",
            "href": folder_href
        })]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        state
            .storage
            .upsert_folder_journaled(
                crate::storage::FolderUpsertParams {
                    owner,
                    server_id: folder_href,
                    parent_id: "0",
                    display_name: "Side Project",
                    folder_type: 13,
                    backend_kind: "caldav",
                    backend_id: folder_href,
                },
                "folder-add",
            )
            .await
            .unwrap();
        let foreign_href = "jmap://calendar/acct/e-other";
        let foreign_server_id = generate_server_id(state.cfg.hmac_secret(), foreign_href);
        state
            .storage
            .upsert_item_map(
                owner,
                foreign_href,
                foreign_href,
                &foreign_server_id,
                "uid-other",
                "etag",
            )
            .await
            .unwrap();

        let state_collection_id = format!("{folder_href}::dev1");
        let xml = run_calendar_sync_for(&state, folder_href, &state_collection_id, "0").await;
        assert!(xml.contains("uid-mine"), "own event delivered:\n{xml}");
        assert!(
            !xml.contains("<Delete>"),
            "a scoped prime must not delete another calendar's event:\n{xml}"
        );
        assert!(
            state
                .storage
                .get_ews_item_by_server_id(owner, &foreign_server_id)
                .await
                .unwrap()
                .is_some(),
            "the foreign calendar's item_map row must survive the scoped prime"
        );
        assert!(
            state
                .storage
                .list_deleted_since(owner, 0)
                .await
                .unwrap()
                .is_empty(),
            "no tombstone may be recorded for a foreign calendar's event"
        );
    }

    /// A fresh prime (SyncKey 0) on ONE device+collection scope must emit
    /// `<Add>` for every event it delivers, even when the owner-global
    /// `item_map` already holds the row from another device's completed prime
    /// of the SAME calendar. The old decision read the shared `item_map`, so the
    /// second device's prime emitted `<Change>` for an item it never held — the
    /// client then ignored/garbled the calendar. Regression for the per-scope
    /// delivery ledger (`calendar_delivery_map`).
    #[tokio::test]
    async fn scoped_prime_emits_add_when_another_device_already_stored_event() {
        let stub = spawn_calendar_stub(
            vec![],
            vec![stub_event("e1", "uid-e1", "Shared")],
            "query-s",
            "get-s",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;

        // Device A primes the default calendar; its row lands in the shared
        // `item_map`.
        let a = run_calendar_sync_for(&state, "1", "1::devA", "0").await;
        assert!(a.contains("<Add>"), "device A prime must Add:\n{a}");

        // Device B primes the SAME calendar on a fresh stream: it never
        // delivered this event, so it must be an `<Add>` for device B.
        let b = run_calendar_sync_for(&state, "1", "1::devB", "0").await;
        assert!(
            b.contains("<Add>") && !b.contains("<Change>"),
            "a fresh prime must emit Add (not Change) even when another device \
             already stored the event:\n{b}"
        );
    }

    /// An event that belongs to TWO calendars (JMAP `calendarIds` with two
    /// entries) must be delivered as `<Add>` in each calendar's scoped folder —
    /// the second calendar's prime sees the owner-global `item_map` row from the
    /// first, but its own scope never delivered it. Regression for the per-scope
    /// delivery ledger.
    #[tokio::test]
    async fn shared_event_is_added_in_each_scoped_calendar() {
        let folder_a = "/dav/cal/user@example.com/A/";
        let folder_b = "/dav/cal/user@example.com/B/";
        let mut shared = scoped_event("e-shared", "uid-shared", "Shared", "cal-a");
        shared["calendarIds"]["cal-b"] = json!(true);
        let stub = spawn_calendar_stub(vec![], vec![shared], "query-s", "get-s", None)
            .await
            .with_calendars(vec![
                json!({"id": "cal-a", "name": "A", "href": folder_a}),
                json!({"id": "cal-b", "name": "B", "href": folder_b}),
            ]);
        let state = calendar_mutation_state(&stub.base).await;
        let owner = "user@example.com";
        for (href, name) in [(folder_a, "A"), (folder_b, "B")] {
            state
                .storage
                .upsert_folder_journaled(
                    crate::storage::FolderUpsertParams {
                        owner,
                        server_id: href,
                        parent_id: "0",
                        display_name: name,
                        folder_type: 13,
                        backend_kind: "caldav",
                        backend_id: href,
                    },
                    "folder-add",
                )
                .await
                .unwrap();
        }

        let a = run_calendar_sync_for(&state, folder_a, &format!("{folder_a}::dev1"), "0").await;
        assert!(
            a.contains("<Add>") && a.contains("uid-shared"),
            "calendar A prime must Add the shared event:\n{a}"
        );
        let b = run_calendar_sync_for(&state, folder_b, &format!("{folder_b}::dev1"), "0").await;
        assert!(
            b.contains("<Add>") && b.contains("uid-shared") && !b.contains("<Change>"),
            "calendar B prime must Add the shared event (its own scope never \
             delivered it), not Change:\n{b}"
        );
    }

    /// Upgrade/continuity: a scope that has no delivery-ledger rows yet (the
    /// ledger table is new, or was never written for this scope) treats its
    /// first DELTA's change set as new to the stream and re-delivers it as
    /// `<Add>` — including an event the client already holds (its shared
    /// `item_map` ETag matching does NOT suppress it, which is exactly what keeps
    /// a per-collection change from being silently dropped for a scope that has
    /// not recorded it). EAS keys items by ServerId, so re-`<Add>` updates the
    /// client's existing copy in place. A per-collection change is likewise
    /// re-reported as `<Add>` — never skipped.
    #[tokio::test]
    async fn delta_without_ledger_redelivers_change_set_as_add() {
        // Delta reports one event already held (e1) and one new event (e2).
        let page = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": false,
            "created": [], "updated": ["e1", "e2"], "destroyed": []
        });
        let e1 = stub_event("e1", "uid-e1", "Event One");
        let e2 = stub_event("e2", "uid-e2", "Event Two");
        let stub = spawn_calendar_stub(vec![page], vec![e1.clone(), e2], "q", "get-s", None).await;
        let state = calendar_sync_state(&stub.base).await;
        let owner = "user@example.com";

        // The client already holds e1 (shared item_map row, CURRENT etag), but NO
        // delivery-ledger row exists for this scope yet.
        let href_e1 = "jmap://calendar/acct/e1";
        let sid_e1 = generate_server_id(state.cfg.hmac_secret(), href_e1);
        let etag_e1 = crate::jmap::calendar_event_etag(&e1, None);
        state
            .storage
            .upsert_item_map(owner, href_e1, href_e1, &sid_e1, "uid-e1", &etag_e1)
            .await
            .unwrap();
        state
            .storage
            .set_sync_key(owner, "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        let xml = run_calendar_sync(&state, "key-1").await;
        assert!(
            xml.contains("uid-e1") && xml.contains("uid-e2"),
            "the whole delta change set is delivered without a ledger:\n{xml}"
        );
        assert!(
            !xml.contains("<Change>"),
            "an empty ledger treats every event as new to the stream (no Change):\n{xml}"
        );

        // Once the ledger is recorded, a repeat delta with the same ETags skips
        // both events (no churn).
        let page2 = json!({
            "accountId": "acct", "oldState": "s1", "newState": "s2",
            "hasMoreChanges": false,
            "created": [], "updated": ["e1", "e2"], "destroyed": []
        });
        stub.set_changes(vec![page2]);
        let xml2 = run_calendar_sync(&state, "key-2").await;
        assert!(
            !xml2.contains("uid-e1") && !xml2.contains("uid-e2"),
            "unchanged events recorded in the ledger are skipped:\n{xml2}"
        );
    }

    /// The delivery ledger and the SyncKey the client is handed advance in the
    /// SAME commit (the P1 fix): after a successful Sync the scope's ledger
    /// records exactly the delivered events, and the stored SyncKey is the new
    /// key — never a ledger ahead of a stale SyncKey (which would make the
    /// client's retry skip an event it never received).
    #[tokio::test]
    async fn successful_sync_advances_ledger_and_sync_key_together() {
        let page = json!({
            "accountId": "acct", "oldState": "s0", "newState": "s1",
            "hasMoreChanges": false,
            "created": ["e1"], "updated": [], "destroyed": []
        });
        let e1 = stub_event("e1", "uid-e1", "Event One");
        let stub = spawn_calendar_stub(vec![page], vec![e1.clone()], "q", "get-s", None).await;
        let state = calendar_sync_state(&stub.base).await;
        let owner = "user@example.com";
        state
            .storage
            .set_sync_key(owner, "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        let xml = run_calendar_sync(&state, "key-1").await;
        assert!(xml.contains("uid-e1"), "the delta must deliver e1:\n{xml}");

        // The response's SyncKey is persisted (a different key from key-1).
        let (stored_key, _) = state
            .storage
            .get_sync_key(owner, "calendar")
            .await
            .unwrap()
            .expect("sync key persisted");
        assert_ne!(stored_key, "key-1", "a completed sync rotates the SyncKey");

        // And the scope ledger records the delivered event with its ETag.
        let href = "jmap://calendar/acct/e1";
        let sid = generate_server_id(state.cfg.hmac_secret(), href);
        let ledger = state
            .storage
            .list_calendar_deliveries(owner, "calendar")
            .await
            .unwrap();
        assert_eq!(
            ledger.get(&sid).map(String::as_str),
            Some(crate::jmap::calendar_event_etag(&e1, None).as_str()),
            "the ledger row is written by the same commit that stores the SyncKey"
        );

        // The delivery also journals a change (`op='upsert'`) so Ping,
        // GetItemEstimate and EWS SyncFolderItems detect the event — without it
        // an event first delivered here would never wake another client.
        let journal = state
            .storage
            .list_journal_since_seq(owner, 0)
            .await
            .unwrap();
        assert!(
            journal
                .iter()
                .any(|r| r.server_id == sid && r.op == "upsert"),
            "a delivered event must journal a change for other clients"
        );
    }

    /// The DEFAULT (unscoped) calendar folder owns the whole account feed, so a
    /// destroyed id IS tombstoned — the Finding E guard must not over-suppress.
    #[tokio::test]
    async fn unscoped_calendar_delta_tombstones_destroyed_event() {
        let stub = spawn_calendar_stub(
            vec![json!({
                "accountId": "acct", "oldState": "s0", "newState": "s1",
                "hasMoreChanges": false,
                "created": [], "updated": [], "destroyed": ["e1"]
            })],
            vec![stub_event("e1", "uid-e1", "Event One")],
            "query-s",
            "get-s",
            None,
        )
        .await;
        let state = calendar_sync_state(&stub.base).await;
        let owner = "user@example.com";
        let href = "jmap://calendar/acct/e1";
        let server_id = generate_server_id(state.cfg.hmac_secret(), href);
        state
            .storage
            .upsert_item_map(owner, href, href, &server_id, "uid-e1", "etag")
            .await
            .unwrap();
        state
            .storage
            .set_sync_key(owner, "calendar", "key-1", Some("s0"))
            .await
            .unwrap();

        let xml = run_calendar_sync(&state, "key-1").await;
        assert!(
            xml.contains(&format!(
                "<Delete><ServerId>{server_id}</ServerId></Delete>"
            )),
            "the default calendar must deliver the deletion:\n{xml}"
        );
        assert!(
            state
                .storage
                .get_ews_item_by_server_id(owner, &server_id)
                .await
                .unwrap()
                .is_none(),
            "the deleted event's item_map row must be removed"
        );
    }

    /// A transient `folder_map` read failure while resolving a custom calendar
    /// folder must FAIL the Sync closed, not be conflated with "unmapped" (which
    /// `perform_sync` treats as an unscoped collection and would serve the
    /// default calendar's / account-wide events under the custom folder). The
    /// failure is induced by dropping the `folder_map` table after the device
    /// has primed its sync key, so the request reaches the resolver.
    #[tokio::test]
    async fn custom_calendar_folder_resolution_failure_fails_closed() {
        let stub = spawn_calendar_stub(vec![], vec![], "query-s", "get-s", None).await;
        let state = calendar_sync_state(&stub.base).await;
        let owner = "user@example.com";
        let folder_href = "/dav/cal/user@example.com/Side%20Project/";
        let state_collection_id = format!("{folder_href}::dev1");
        // A primed device: the incoming non-zero key matches the stored key, so
        // the request proceeds past the invalid-key guard to the folder
        // resolver.
        state
            .storage
            .set_sync_key(owner, &state_collection_id, "key-1", Some("s0"))
            .await
            .unwrap();
        // Poison the `folder_map` read.
        sqlx::query("DROP TABLE folder_map")
            .execute(state.storage.pool())
            .await
            .unwrap();

        let opts = SyncOptions {
            window_size: 100,
            get_changes: true,
            filter_start: Utc::now() - Duration::weeks(52),
            body_options: crate::eas_sync_options::EasSyncCollectionOptions::default(),
        };
        let result = perform_sync(&PerformSyncParams {
            state: state.clone(),
            owner,
            collection_id: folder_href,
            state_collection_id: &state_collection_id,
            incoming_sync_key: "key-1",
            content_class: "Calendar",
            opts,
            username: owner,
            password: "pw",
            client_mutation_responses: "",
        })
        .await;
        assert!(
            result.is_err(),
            "a folder_map read failure must fail the Sync closed, got: {result:?}"
        );
    }
}
