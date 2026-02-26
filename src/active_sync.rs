// src/active_sync.rs
use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use lettre::{SmtpTransport, Transport};
use quick_xml::events::Event;
use quick_xml::Reader;
use quick_xml::escape;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct SyncRequest {
    #[serde(rename = "Collections")]
    collections: Collections,
}

#[derive(Debug, Serialize, Deserialize)]
struct Collections {
    #[serde(rename = "Collection")]
    collection: Collection,
}

#[derive(Debug, Serialize, Deserialize)]
struct Collection {
    #[serde(rename = "SyncKey")]
    sync_key: String,
    #[serde(rename = "CollectionId")]
    collection_id: String,
    #[serde(rename = "Commands", skip_serializing_if = "Option::is_none")]
    commands: Option<Commands>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Commands {
    #[serde(rename = "Add", skip_serializing_if = "Option::is_none")]
    add: Option<Vec<AddCommand>>,
    #[serde(rename = "Change", skip_serializing_if = "Option::is_none")]
    change: Option<Vec<ChangeCommand>>,
    #[serde(rename = "Delete", skip_serializing_if = "Option::is_none")]
    delete: Option<Vec<DeleteCommand>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct AddCommand {
    #[serde(rename = "ClientId")]
    client_id: String,
    #[serde(rename = "ApplicationData")]
    application_data: ApplicationData,
}

#[derive(Debug, Serialize, Deserialize)]
struct ChangeCommand {
    #[serde(rename = "ServerId")]
    server_id: String,
    #[serde(rename = "ApplicationData")]
    application_data: ApplicationData,
}

#[derive(Debug, Serialize, Deserialize)]
struct DeleteCommand {
    #[serde(rename = "ServerId")]
    server_id: String,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ApplicationData {
    #[serde(rename = "Subject", skip_serializing_if = "Option::is_none")]
    subject: Option<String>,
    #[serde(rename = "Location", skip_serializing_if = "Option::is_none")]
    location: Option<String>,
    #[serde(rename = "Start", skip_serializing_if = "Option::is_none")]
    start: Option<String>,
    #[serde(rename = "End", skip_serializing_if = "Option::is_none")]
    end: Option<String>,
    #[serde(rename = "Body", skip_serializing_if = "Option::is_none")]
    body: Option<BodyData>,
    #[serde(rename = "UID", skip_serializing_if = "Option::is_none")]
    uid: Option<String>,
    #[serde(rename = "Attendees", skip_serializing_if = "Option::is_none")]
    attendees: Option<AttendeesList>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BodyData {
    #[serde(rename = "Data")]
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct AttendeesList {
    #[serde(rename = "Attendee")]
    items: Vec<Attendee>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Attendee {
    #[serde(rename = "Email")]
    email: String,
    #[serde(rename = "Name")]
    name: String,
}

pub async fn process_request(config: &AppConfig, xml: &str, headers: &HeaderMap) -> String {
    let mut buf = Vec::new();
    let mut command = String::new();
    let mut depth = 0;
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                depth += 1;
                if depth == 2 {
                    command = std::str::from_utf8(e.local_name().as_ref()).unwrap_or("").to_string();
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let auth = headers.get("Authorization").unwrap().to_str().unwrap();
    let (user, pass) = utils::decode_basic_auth(auth);
    let device_id = headers.get("X-MS-DeviceId")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("unknown");

    let session = match jmap_client::get_session(&config.jmap_url, &user, &pass).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("JMAP Auth failed: {}", e);
            return error_xml(500, "AuthFailed");
        }
    };

    match command.as_str() {
        "FolderSync" => handle_folder_sync(&session, config, &user, device_id).await,
        "Sync" => {
            match quick_xml::de::from_str::<SyncRequest>(xml) {
                Ok(req) => handle_sync(&session, config, &user, device_id, req).await,
                Err(e) => {
                    tracing::error!("XML Parse Error: {:?}", e);
                    error_xml(400, "BadRequest")
                }
            }
        }
        "Ping" => handle_ping().await,
        "MeetingResponse" => handle_meeting_response(&session, config, xml, &user).await,
        "Settings" => handle_settings(&session, config, &user, device_id).await,
        "Provision" => handle_provision().await,
        "SendMail" => handle_send_mail(&session, config, &user, xml).await,
        _ => error_xml(400, "UnsupportedCommand"),
    }
}

async fn handle_provision() -> String {
    r#"<Provision xmlns="Provision:">
        <Status>1</Status>
        <Policies>
            <Policy>
                <PolicyType>MS-EAS-Provisioning-WBXML</PolicyType>
                <Status>1</Status>
                <PolicyKey>1234567890</PolicyKey>
                <Data>
                    <DevicePasswordEnabled>0</DevicePasswordEnabled>
                </Data>
            </Policy>
        </Policies>
    </Provision>"#.to_string()
}

async fn handle_send_mail(_session: &jmap_client::JmapSession, config: &AppConfig, _user: &str, xml: &str) -> String {
    let mut mime_content = String::new();
    let mut buf = Vec::new();
    let mut in_mime = false;
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") == "Mime" {
                    in_mime = true;
                }
            }
            Ok(Event::End(ref e)) => {
                if std::str::from_utf8(e.local_name().as_ref()).unwrap_or("") == "Mime" {
                    in_mime = false;
                }
            }
            Ok(Event::Text(t)) => {
                if in_mime {
                    // Fix: use escape::unescape free function
                    mime_content.push_str(&escape::unescape(&t).unwrap_or_default());
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    // Parse To and From from MIME using Regex
    let re_to = Regex::new(r"To:\s*(.*?)\r?\n").unwrap();
    let re_from = Regex::new(r"From:\s*(.*?)\r?\n").unwrap();
    
    let to_addr = re_to.captures(&mime_content).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string());
    let from_addr = re_from.captures(&mime_content).and_then(|c| c.get(1)).map(|m| m.as_str().trim().to_string());

    let status = if let (Some(to), Some(from)) = (to_addr, from_addr) {
        // Build Email via lettre
        let email_builder = lettre::Message::builder()
            .from(from.parse().unwrap())
            .to(to.parse().unwrap());
        
        // Extract Subject
        let re_subj = Regex::new(r"Subject:\s*(.*?)\r?\n").unwrap();
        let subject = re_subj.captures(&mime_content).and_then(|c| c.get(1)).map(|m| m.as_str().to_string()).unwrap_or_default();
        
        let email = email_builder
            .subject(subject)
            .body(mime_content) 
            .unwrap();

        // Parse SMTP URL from config
        let smtp_url = url::Url::parse(&config.smtp_url).unwrap();
        let host = smtp_url.host_str().unwrap();
        let port = smtp_url.port().unwrap_or(25);

        let mailer = SmtpTransport::builder_dangerous(host).port(port).build();
        
        match mailer.send(&email) {
            Ok(_) => "1",
            Err(e) => { tracing::error!("SMTP Error: {}", e); "2" }
        }
    } else {
        "2"
    };

    format!(r#"<SendMail xmlns="AirSync:"><Status>{}</Status></SendMail>"#, status)
}

async fn handle_ping() -> String {
    r#"<Ping xmlns="Ping:"><Status>1</Status></Ping>"#.to_string()
}

async fn handle_settings(_session: &jmap_client::JmapSession, config: &AppConfig, user: &str, device_id: &str) -> String {
    db::register_device(config, user, device_id).await;
    format!(r#"<Settings xmlns="Settings:"><Status>1</Status></Settings>"#)
}

async fn handle_folder_sync(session: &jmap_client::JmapSession, config: &AppConfig, user: &str, device_id: &str) -> String {
    db::register_device(config, user, device_id).await;
    
    let cal_id = match jmap_client::get_default_calendar_id(&session.api_url, &session.access_token, &session.account_id).await {
        Ok(id) => id,
        Err(_) => "calendar-default".to_string(),
    };

    format!(
        r#"<FolderSync xmlns="AirSync:">
            <Status>1</Status>
            <Collections>
                <Collection>
                    <SyncKey>0</SyncKey>
                    <ServerId>{}</ServerId>
                    <ParentId>0</ParentId>
                    <DisplayName>Calendar</DisplayName>
                    <Type>8</Type>
                </Collection>
            </Collections>
        </FolderSync>"#, cal_id
    )
}

async fn handle_sync(session: &jmap_client::JmapSession, config: &AppConfig, user: &str, device_id: &str, req: SyncRequest) -> String {
    let coll = req.collections.collection;
    
    // Clone the key to avoid move error
    let old_sync_key = coll.sync_key.clone();

    if let Some(cmds) = coll.commands {
        process_client_commands(session, cmds, &config.timezone).await;
    }

    let current_jmap_state = match jmap_client::get_calendar_state(&session.api_url, &session.access_token, &session.account_id).await {
        Ok(s) => s,
        Err(_) => return error_xml(500, "JMAPStateError"),
    };

    let prev_state = db::get_sync_state(config, user, device_id, &coll.collection_id).await;
    
    let (items_xml, new_sync_key) = if old_sync_key == "0" || prev_state.is_none() {
        let events = jmap_client::get_calendar_events(&session.api_url, &session.access_token, &session.account_id).await.unwrap_or_default();
        render_items(&events, "Add", &config.timezone)
    } else {
        let prev_jmap_state = prev_state.unwrap();
        if prev_jmap_state == current_jmap_state {
            (String::new(), old_sync_key.clone())
        } else {
            let changes = jmap_client::get_calendar_changes(&session.api_url, &session.access_token, &session.account_id, &prev_jmap_state).await.unwrap_or_default();
            render_changes(&session.api_url, &session.access_token, &session.account_id, changes, &config.timezone).await
        }
    };

    let final_sync_key = if new_sync_key != old_sync_key {
        db::update_sync_state(config, user, device_id, &coll.collection_id, &new_sync_key, &current_jmap_state).await;
        new_sync_key
    } else {
        new_sync_key
    };

    format!(
        r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:">
            <Collections>
                <Collection>
                    <SyncKey>{}</SyncKey>
                    <CollectionId>{}</CollectionId>
                    <Status>1</Status>
                    <Commands>{}</Commands>
                </Collection>
            </Collections>
        </Sync>"#,
        final_sync_key, coll.collection_id, items_xml
    )
}

fn render_items(events: &[jmap_client::JmapEvent], mode: &str, tz_str: &str) -> (String, String) {
    let mut xml = String::new();
    let new_key = uuid::Uuid::new_v4().to_string();
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    
    for event in events {
        let start_dt: DateTime<Utc> = event.start.parse().unwrap_or_default();
        let end_dt: DateTime<Utc> = event.end.parse().unwrap_or_default();
        
        let start_local = start_dt.with_timezone(&tz);
        let end_local = end_dt.with_timezone(&tz);

        xml.push_str(&format!(
            r#"<{}><ServerId>{}</ServerId><ApplicationData>
                <Calendar:Subject>{}</Calendar:Subject>
                <Calendar:Location>{}</Calendar:Location>
                <Calendar:Start>{}</Calendar:Start>
                <Calendar:End>{}</Calendar:End>
                <Calendar:UID>{}</Calendar:UID>
                <AirSyncBase:Body>
                    <AirSyncBase:Type>1</AirSyncBase:Type>
                    <AirSyncBase:Data>{}</AirSyncBase:Data>
                </AirSyncBase:Body>
            </ApplicationData></{}>"#,
            mode, 
            event.id.as_deref().unwrap_or(""), 
            event.title, 
            event.location.as_deref().unwrap_or(""), 
            start_local.format("%Y-%m-%dT%H:%M:%S"), 
            end_local.format("%Y-%m-%dT%H:%M:%S"),
            event.uid.as_deref().unwrap_or(""),
            event.description.as_deref().unwrap_or(""),
            mode
        ));
    }
    (xml, new_key)
}

async fn render_changes(url: &str, token: &str, account_id: &str, changes: jmap_client::JmapChanges, tz_str: &str) -> (String, String) {
    let mut xml = String::new();
    let new_key = uuid::Uuid::new_v4().to_string();

    for id in &changes.destroyed {
        xml.push_str(&format!(r#"<Delete><ServerId>{}</ServerId></Delete>"#, id));
    }
    
    if !changes.updated.is_empty() {
        if let Ok(events) = jmap_client::get_events_by_ids(url, token, account_id, &changes.updated).await {
            let (change_xml, _) = render_items(&events, "Change", tz_str);
            xml.push_str(&change_xml);
        }
    }
    (xml, new_key)
}

async fn process_client_commands(session: &jmap_client::JmapSession, cmds: Commands, tz_str: &str) {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    for add_cmd in cmds.add.unwrap_or_default() {
        let data = add_cmd.application_data;
        
        let start_utc = parse_local_to_utc(&data.start.unwrap_or_default(), tz);
        let end_utc = parse_local_to_utc(&data.end.unwrap_or_default(), tz);

        let attendees: Vec<jmap_client::Participant> = data.attendees.map(|a| a.items.into_iter().map(|att| jmap_client::Participant {
            email: att.email,
            name: att.name,
        }).collect()).unwrap_or_default();

        let event = jmap_client::JmapEvent {
            id: None,
            title: data.subject.unwrap_or_default(),
            start: start_utc,
            end: end_utc,
            location: data.location,
            description: data.body.map(|b| b.data),
            uid: data.uid,
            participants: if attendees.is_empty() { None } else { Some(attendees) },
            is_all_day: false,
        };
        let _ = jmap_client::push_event(&session.api_url, &session.access_token, &session.account_id, event).await;
    }
}

fn parse_local_to_utc(local_str: &str, tz: Tz) -> String {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%S") {
        return tz.from_local_datetime(&dt).single().map(|dt| dt.with_timezone(&Utc).to_rfc3339()).unwrap_or_default();
    }
    local_str.to_string()
}

async fn handle_meeting_response(session: &jmap_client::JmapSession, _config: &AppConfig, xml: &str, user_email: &str) -> String {
    let mut uid = String::new();
    let mut response_code = 0;
    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => current_tag = std::str::from_utf8(e.local_name().as_ref()).unwrap_or("").to_string(),
            Ok(Event::Text(t)) => {
                let text = escape::unescape(&t).unwrap_or_default();
                match current_tag.as_str() {
                    "RequestId" => uid = text.to_string(),
                    "UserResponse" => response_code = text.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let event_id = match jmap_client::find_event_by_uid(&session.api_url, &session.access_token, &session.account_id, &uid).await {
        Ok(id) => id,
        Err(_) => return error_xml(400, "EventNotFound"),
    };

    let status_str = match response_code {
        1 => "accepted",
        2 => "tentative",
        3 => "declined",
        _ => "needs-action",
    };

    let _ = jmap_client::update_participant_status(&session.api_url, &session.access_token, &session.account_id, &event_id, user_email, status_str).await;

    format!(
        r#"<MeetingResponse xmlns="AirSync:">
            <Result>
                <Status>1</Status>
                <CalendarId>{}</CalendarId>
            </Result>
        </MeetingResponse>"#, event_id
    )
}

fn error_xml(code: i32, msg: &str) -> String {
    format!("<Error><Code>{}</Code><Message>{}</Message></Error>", code, msg)
}
