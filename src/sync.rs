// src/sync.rs
use crate::caldav::CaldavClient;
use crate::calendar::{
    Attendee, EasSyncMutation, parse_eas_sync_mutations, parse_ics_event, render_ics,
};
use crate::models::AppState;
use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{TimeZone, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub fn generate_server_id(secret: &str, resource_href: &str) -> String {
    let key = secret.as_bytes();
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC init");
    mac.update(resource_href.as_bytes());
    let result = mac.finalize().into_bytes();
    URL_SAFE_NO_PAD.encode(result)
}

pub async fn apply_client_sync_mutations(
    state: Arc<AppState>,
    owner: &str,
    username: &str,
    password: &str,
    xml: &str,
) -> Result<()> {
    let mutations = parse_eas_sync_mutations(xml)?;
    if mutations.is_empty() {
        return Ok(());
    }

    let caldav = CaldavClient::new(&state.cfg);
    let calendars = caldav.find_user_calendars(username, password).await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow!("no calendars found"))?
        .clone();

    for mutation in mutations {
        match mutation {
            EasSyncMutation::Add { client_id, item } => {
                let ics = render_ics(&item);
                let (resource_href, etag) = caldav
                    .put_event(&collection_href, None, &ics, username, password, None)
                    .await?;
                let server_id = generate_server_id(&state.cfg.hmac_secret, &resource_href);
                let uid = if item.uid.is_empty() {
                    client_id.unwrap_or_else(|| Uuid::new_v4().to_string())
                } else {
                    item.uid
                };
                state
                    .storage
                    .upsert_item_map(
                        owner,
                        &collection_href,
                        &resource_href,
                        &server_id,
                        &uid,
                        &etag,
                    )
                    .await?;
            }
            EasSyncMutation::Change { server_id, patch } => {
                let Some(existing) = state
                    .storage
                    .get_ews_item_by_server_id(owner, &server_id)
                    .await?
                else {
                    return Err(anyhow!("unknown server id in EAS Change: {server_id}"));
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
                if let Some(v) = patch.rrule {
                    item.rrule = Some(v);
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
                if let Some(v) = patch.response_requested {
                    item.response_requested = Some(v);
                }
                if let Some(v) = patch.meeting_status {
                    item.meeting_status = Some(v);
                }
                if let Some(v) = patch.response_type {
                    item.response_type = Some(v);
                }

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
                        &server_id,
                        &item.uid,
                        &etag,
                    )
                    .await?;
            }
            EasSyncMutation::Delete { server_id } => {
                let Some(existing) = state
                    .storage
                    .get_ews_item_by_server_id(owner, &server_id)
                    .await?
                else {
                    continue;
                };
                caldav
                    .delete_event(
                        &existing.resource_href,
                        username,
                        password,
                        existing.etag.as_deref(),
                    )
                    .await?;
                state
                    .storage
                    .delete_item_by_server_id(owner, &server_id)
                    .await?;
            }
        }
    }

    Ok(())
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

    let ics = render_ics(&item);
    let (resource_href, etag) = caldav
        .put_event(
            &existing.resource_href,
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
            &resource_href,
            &resource_href,
            request_id,
            &item.uid,
            &etag,
        )
        .await?;
    Ok(())
}

fn xml_escape(input: &str) -> String {
    input
        .replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
}

fn parse_ics_content(ics: &str) -> Vec<(String, String)> {
    let mut properties = Vec::new();
    let unfolded = ics.replace("\r\n ", "").replace("\r\n\t", "");
    for line in unfolded.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(colon_idx) = line.find(':') {
            let key = line[..colon_idx].to_string();
            let value = line[colon_idx + 1..].to_string();
            properties.push((key, value));
        }
    }
    properties
}

fn parse_datetime(val: &str) -> Option<chrono::DateTime<Utc>> {
    if val.ends_with('Z') {
        chrono::NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%SZ")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
    } else if val.contains('T') {
        chrono::NaiveDateTime::parse_from_str(val, "%Y%m%dT%H%M%S")
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
    } else {
        chrono::NaiveDate::parse_from_str(val, "%Y%m%d")
            .map(|d| d.and_hms_opt(0, 0, 0).unwrap())
            .map(|dt| Utc.from_utc_datetime(&dt))
            .ok()
    }
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
        _ => "".to_string(),
    }
}

fn map_rrule_to_recurrence_xml(
    props: &[(String, String)],
    _dtstart: &chrono::DateTime<Utc>,
) -> Option<String> {
    let mut rrule_str = String::new();
    for (k, v) in props {
        if k.starts_with("RRULE") {
            rrule_str = v.clone();
            break;
        }
    }
    if rrule_str.is_empty() {
        return None;
    }

    let parts: Vec<&str> = rrule_str.split(';').collect();
    let mut freq: Option<u8> = None;
    let mut interval = 1u32;
    let mut day_of_week = String::new();
    let mut day_of_month = 1u32;
    let mut month_of_year = 1u32;
    let mut week_of_month = 0u32;
    let mut until: Option<String> = None;
    let mut occurrences: Option<u32> = None;

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
                        if d.len() > 2
                            && let Ok(wk) = d[..d.len() - 2].parse::<u32>()
                        {
                            week_of_month = wk;
                        }
                        let day_code = &d[d.len() - 2..];
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
                _ => {}
            }
        }
    }

    let freq_val = freq?;
    let mut xml = "<Calendar:Recurrence>".to_string();
    xml.push_str(&format!("<Calendar:Type>{}</Calendar:Type>", freq_val));
    xml.push_str(&format!(
        "<Calendar:Interval>{}</Calendar:Interval>",
        interval
    ));

    if freq_val == 1 {
        // Weekly
        if !day_of_week.is_empty() {
            xml.push_str(&format!(
                "<Calendar:DayOfWeek>{}</Calendar:DayOfWeek>",
                day_of_week
            ));
        }
    }

    if freq_val == 2 || freq_val == 5 {
        xml.push_str(&format!(
            "<Calendar:DayOfMonth>{}</Calendar:DayOfMonth>",
            day_of_month
        ));
        if freq_val == 5 {
            xml.push_str(&format!(
                "<Calendar:MonthOfYear>{}</Calendar:MonthOfYear>",
                month_of_year
            ));
        }
    }

    if week_of_month > 0 {
        if freq_val == 2 {
            xml = xml.replace(
                "<Calendar:Type>2</Calendar:Type>",
                "<Calendar:Type>3</Calendar:Type>",
            );
        }
        if freq_val == 5 {
            xml = xml.replace(
                "<Calendar:Type>5</Calendar:Type>",
                "<Calendar:Type>6</Calendar:Type>",
            );
        }
        xml.push_str(&format!(
            "<Calendar:WeekOfMonth>{}</Calendar:WeekOfMonth>",
            week_of_month
        ));
        if !day_of_week.is_empty() {
            xml.push_str(&format!(
                "<Calendar:DayOfWeek>{}</Calendar:DayOfWeek>",
                day_of_week
            ));
        }
    }

    if let Some(u) = until {
        xml.push_str(&format!("<Calendar:Until>{}</Calendar:Until>", u));
    } else if let Some(o) = occurrences {
        xml.push_str(&format!(
            "<Calendar:Occurrences>{}</Calendar:Occurrences>",
            o
        ));
    }

    xml.push_str("</Calendar:Recurrence>");
    Some(xml)
}

pub async fn perform_sync(
    state: Arc<AppState>,
    owner: &str,
    collection_id: &str,
    _incoming_sync_key: &str,
    content_class: &str,
    _window_size: usize,
    username: &str,
    password: &str,
) -> Result<String> {
    let storage = &state.storage;

    // Class-aware sync surface for high-priority non-calendar ActiveSync classes.
    if !content_class.eq_ignore_ascii_case("Calendar") {
        let class_name = content_class.trim();
        let normalized = if class_name.is_empty() {
            "Calendar"
        } else {
            class_name
        };
        let new_sync_key = Uuid::new_v4().to_string();
        storage
            .set_sync_key(owner, collection_id, &new_sync_key, Some("token"))
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

        let xml = format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Contacts="Contacts:" xmlns:Tasks="Tasks:" xmlns:Notes="Notes:" xmlns:DocumentLibrary="DocumentLibrary:" xmlns:RightsManagement="RightsManagement:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>{}</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>1</Status>
<Commands>{}</Commands>
</Collection></Collections>
</Sync>"#,
            xml_escape(normalized),
            new_sync_key,
            xml_escape(collection_id),
            commands
        );
        return Ok(xml);
    }

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

    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(&events_xml);
    reader.config_mut().trim_text(true);

    #[derive(Clone)]
    struct EventItem {
        href: String,
        etag: String,
        ics: String,
    }
    let mut events = Vec::new();
    let mut current = EventItem {
        href: String::new(),
        etag: String::new(),
        ics: String::new(),
    };
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                match e.name().local_name().as_ref() {
                    b"href" => {
                        if let Ok(Event::Text(e)) = reader.read_event_into(&mut buf) {
                            // FIXED: Use decode() for quick-xml 0.39
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
                }
            }
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

    let old_items = storage.list_changes_since(owner, 0).await?;
    let mut commands = String::new();
    let mut seen_ids = Vec::new();

    for ev in events {
        let href = ev.href;
        if href.is_empty() {
            continue;
        }
        let resource_href = href.clone();
        let etag = ev.etag.trim_matches('"').to_string();
        let ics = ev.ics;
        let server_id = generate_server_id(&state.cfg.hmac_secret, &resource_href);
        seen_ids.push(server_id.clone());

        let props = parse_ics_content(&ics);
        let mut subject = "Event".to_string();
        let mut dtstart = Utc::now();
        let mut dtend = Utc::now() + chrono::Duration::hours(1);
        let mut location = String::new();
        let mut description = String::new();
        let mut uid = Uuid::new_v4().to_string();
        let mut is_all_day = false;
        let mut organizer_name = String::new();
        let mut organizer_email = String::new();
        let mut attendees: Vec<(String, String, u8, u8)> = Vec::new();
        let mut meeting_status: Option<String> = None;
        let mut response_type: Option<String> = None;
        let mut response_requested: Option<String> = None;

        for (key, val) in &props {
            if key.starts_with("SUMMARY") {
                subject = val.clone();
            } else if key.starts_with("DTSTART") {
                dtstart = parse_datetime(val).unwrap_or_else(Utc::now);
                if !val.contains('T') {
                    is_all_day = true;
                }
            } else if key.starts_with("DTEND") {
                dtend =
                    parse_datetime(val).unwrap_or_else(|| Utc::now() + chrono::Duration::hours(1));
            } else if key.starts_with("LOCATION") {
                location = val.clone();
            } else if key.starts_with("DESCRIPTION") {
                description = val.clone();
            } else if key.starts_with("UID") {
                uid = val.clone();
            } else if key.starts_with("ORGANIZER") {
                organizer_email = val
                    .strip_prefix("mailto:")
                    .or_else(|| val.strip_prefix("MAILTO:"))
                    .unwrap_or(val)
                    .to_string();
                for part in key.split(';').skip(1) {
                    if let Some((k, v)) = part.split_once('=')
                        && k.eq_ignore_ascii_case("CN")
                    {
                        organizer_name = v.to_string();
                    }
                }
            } else if key.starts_with("ATTENDEE") {
                let email = val
                    .strip_prefix("mailto:")
                    .or_else(|| val.strip_prefix("MAILTO:"))
                    .unwrap_or(val)
                    .to_string();
                let mut name = String::new();
                let mut attendee_type = 1u8;
                let mut attendee_status = 5u8;
                for part in key.split(';').skip(1) {
                    if let Some((k, v)) = part.split_once('=') {
                        if k.eq_ignore_ascii_case("CN") {
                            name = v.to_string();
                        } else if k.eq_ignore_ascii_case("ROLE") {
                            attendee_type = match v {
                                "OPT-PARTICIPANT" => 2,
                                "NON-PARTICIPANT" => 3,
                                _ => 1,
                            };
                        } else if k.eq_ignore_ascii_case("PARTSTAT") {
                            attendee_status = match v {
                                "TENTATIVE" => 2,
                                "ACCEPTED" => 3,
                                "DECLINED" => 4,
                                _ => 5,
                            };
                        }
                    }
                }
                attendees.push((email, name, attendee_type, attendee_status));
            } else if key.starts_with("X-MS-MEETING-STATUS") {
                meeting_status = Some(val.clone());
            } else if key.starts_with("X-MS-RESPONSE-TYPE") {
                response_type = Some(val.clone());
            } else if key.starts_with("X-MS-RESPONSE-REQUESTED") {
                response_requested = Some(if val.eq_ignore_ascii_case("TRUE") {
                    "1".to_string()
                } else {
                    "0".to_string()
                });
            }
        }

        storage
            .upsert_item_map(
                owner,
                &collection_href,
                &resource_href,
                &server_id,
                &uid,
                &etag,
            )
            .await?;
        let is_new = !old_items.iter().any(|(id, _)| id == &server_id);

        if is_new {
            commands.push_str("<Add><ServerId>");
        } else {
            commands.push_str("<Change><ServerId>");
        }
        commands.push_str(&server_id);
        commands.push_str("</ServerId><ApplicationData>");

        commands.push_str(&format!(
            "<Calendar:Subject>{}</Calendar:Subject>",
            xml_escape(&subject)
        ));
        commands.push_str(&format!(
            "<Calendar:StartTime>{}</Calendar:StartTime>",
            dtstart.format("%Y-%m-%dT%H:%M:%SZ")
        ));
        commands.push_str(&format!(
            "<Calendar:EndTime>{}</Calendar:EndTime>",
            dtend.format("%Y-%m-%dT%H:%M:%SZ")
        ));
        commands.push_str(&format!(
            "<Calendar:DtStamp>{}</Calendar:DtStamp>",
            Utc::now().format("%Y-%m-%dT%H:%M:%SZ")
        ));
        commands.push_str("<Calendar:BusyStatus>2</Calendar:BusyStatus><Calendar:Sensitivity>0</Calendar:Sensitivity>");

        if !location.is_empty() {
            commands.push_str(&format!(
                "<Calendar:Location>{}</Calendar:Location>",
                xml_escape(&location)
            ));
        }
        if !organizer_name.is_empty() {
            commands.push_str(&format!(
                "<Calendar:OrganizerName>{}</Calendar:OrganizerName>",
                xml_escape(&organizer_name)
            ));
        }
        if !organizer_email.is_empty() {
            commands.push_str(&format!(
                "<Calendar:OrganizerEmail>{}</Calendar:OrganizerEmail>",
                xml_escape(&organizer_email)
            ));
        }
        if !attendees.is_empty() {
            commands.push_str("<Calendar:Attendees>");
            for (email, name, attendee_type, attendee_status) in &attendees {
                commands.push_str("<Calendar:Attendee>");
                if !email.is_empty() {
                    commands.push_str(&format!(
                        "<Calendar:Email>{}</Calendar:Email>",
                        xml_escape(email)
                    ));
                }
                if !name.is_empty() {
                    commands.push_str(&format!(
                        "<Calendar:Name>{}</Calendar:Name>",
                        xml_escape(name)
                    ));
                }
                commands.push_str(&format!(
                    "<Calendar:AttendeeType>{}</Calendar:AttendeeType><Calendar:AttendeeStatus>{}</Calendar:AttendeeStatus>",
                    attendee_type, attendee_status
                ));
                commands.push_str("</Calendar:Attendee>");
            }
            commands.push_str("</Calendar:Attendees>");
        }

        let body_len = description.len();
        commands.push_str("<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type>");
        commands.push_str(&format!(
            "<AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>",
            body_len
        ));
        commands.push_str("<AirSyncBase:Truncated>0</AirSyncBase:Truncated><AirSyncBase:Data>");
        commands.push_str(&xml_escape(&description));
        commands.push_str("</AirSyncBase:Data></AirSyncBase:Body>");

        commands.push_str(&format!(
            "<Calendar:UID>{}</Calendar:UID>",
            xml_escape(&uid)
        ));
        commands.push_str(if is_all_day {
            "<Calendar:AllDayEvent>1</Calendar:AllDayEvent>"
        } else {
            "<Calendar:AllDayEvent>0</Calendar:AllDayEvent>"
        });

        if let Some(rec_xml) = map_rrule_to_recurrence_xml(&props, &dtstart) {
            commands.push_str(&rec_xml);
        }
        if let Some(v) = meeting_status {
            commands.push_str(&format!(
                "<Calendar:MeetingStatus>{}</Calendar:MeetingStatus>",
                xml_escape(&v)
            ));
        }
        if let Some(v) = response_type {
            commands.push_str(&format!(
                "<Calendar:ResponseType>{}</Calendar:ResponseType>",
                xml_escape(&v)
            ));
        }
        if let Some(v) = response_requested {
            commands.push_str(&format!(
                "<Calendar:ResponseRequested>{}</Calendar:ResponseRequested>",
                xml_escape(&v)
            ));
        }

        if is_new {
            commands.push_str("</ApplicationData></Add>");
        } else {
            commands.push_str("</ApplicationData></Change>");
        }
    }

    for (old_id, _) in old_items {
        if !seen_ids.contains(&old_id) {
            commands.push_str(&format!("<Delete><ServerId>{}</ServerId></Delete>", old_id));
            let _ = storage.delete_item_by_server_id(owner, &old_id).await;
        }
    }

    let new_sync_key = Uuid::new_v4().to_string();
    storage
        .set_sync_key(owner, collection_id, &new_sync_key, Some("token"))
        .await?;

    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
<Collections><Collection>
<Class>Calendar</Class>
<SyncKey>{}</SyncKey>
<CollectionId>{}</CollectionId>
<Status>1</Status>
<Commands>{}</Commands>
</Collection></Collections>
</Sync>"#,
        new_sync_key, collection_id, commands
    );
    Ok(xml)
}
