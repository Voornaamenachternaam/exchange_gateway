// src/active_sync.rs
use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use lettre::{SmtpTransport, Transport};
use quick_xml::Reader;
use quick_xml::escape;
use quick_xml::events::Event;
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
    #[serde(rename = "Options", skip_serializing_if = "Option::is_none")]
    options: Option<SyncOptions>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct SyncOptions {
    #[serde(rename = "FilterType")]
    filter_type: Option<i32>,
    #[serde(rename = "BodyPreference")]
    body_preference: Option<BodyPreference>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BodyPreference {
    #[serde(rename = "Type")]
    body_type: i32,
    #[serde(rename = "TruncationSize", skip_serializing_if = "Option::is_none")]
    truncation_size: Option<i32>,
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
    #[serde(rename = "Subject", default)]
    subject: Option<String>,
    #[serde(rename = "Location", default)]
    location: Option<String>,
    #[serde(rename = "Start", default)]
    start: Option<String>,
    #[serde(rename = "End", default)]
    end: Option<String>,
    #[serde(rename = "Body", default)]
    body: Option<BodyData>,
    #[serde(rename = "UID", default)]
    uid: Option<String>,
    #[serde(rename = "Attendees", default)]
    attendees: Option<AttendeesList>,
    #[serde(rename = "Reminder", default)]
    reminder: Option<i32>,
    #[serde(rename = "AllDayEvent", default)]
    all_day_event: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BodyData {
    #[serde(rename = "Type")]
    body_type: i32,
    #[serde(rename = "Data")]
    data: String,
    #[serde(rename = "EstimatedDataSize", skip_serializing_if = "Option::is_none")]
    estimated_data_size: Option<i32>,
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
    #[serde(rename = "AttendeeStatus", default)]
    status: Option<String>,
    #[serde(rename = "AttendeeType", default)]
    attendee_type: Option<i32>,
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
                if depth == 1 {
                    command = std::str::from_utf8(e.local_name().as_ref())
                        .unwrap_or("")
                        .to_string();
                }
            }
            Ok(Event::Empty(ref e)) => {
                if depth == 0 {
                    command = std::str::from_utf8(e.local_name().as_ref())
                        .unwrap_or("")
                        .to_string();
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let auth = match headers.get("Authorization").and_then(|h| h.to_str().ok()) {
        Some(a) => a,
        None => return error_xml(401, "Unauthorized"),
    };

    let (user, pass) = utils::decode_basic_auth(auth);
    let device_id = headers
        .get("X-MS-DeviceId")
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
        "Sync" => match quick_xml::de::from_str::<SyncRequest>(xml) {
            Ok(req) => handle_sync(&session, config, &user, device_id, req).await,
            Err(e) => {
                tracing::error!("XML Parse Error: {:?}", e);
                error_xml(400, "BadRequest")
            }
        },
        "Ping" => handle_ping().await,
        "MeetingResponse" => handle_meeting_response(&session, config, xml, &user).await,
        "Settings" => handle_settings(&session, config, &user, device_id).await,
        "Provision" => handle_provision().await,
        "SendMail" => handle_send_mail(&session, config, &user, xml).await,
        "ItemOperations" => handle_item_operations().await,
        "Search" => handle_search().await,
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
                <PolicyKey>987654321</PolicyKey>
                <Data>
                    <DevicePasswordEnabled>0</DevicePasswordEnabled>
                    <RequireDeviceEncryption>0</RequireDeviceEncryption>
                </Data>
            </Policy>
        </Policies>
    </Provision>"#
        .to_string()
}

async fn handle_item_operations() -> String {
    r#"<ItemOperations xmlns="ItemOperations:">
        <Status>1</Status>
        <Response />
    </ItemOperations>"#
        .to_string()
}

async fn handle_search() -> String {
    r#"<Search xmlns="Search:">
        <Status>1</Status>
        <Response>
            <Store>
                <Status>1</Status>
                <Result />
            </Store>
        </Response>
    </Search>"#
        .to_string()
}

async fn handle_send_mail(
    _session: &jmap_client::JmapSession,
    config: &AppConfig,
    _user: &str,
    xml: &str,
) -> String {
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
                    let text_str = std::str::from_utf8(&t).unwrap_or("");
                    mime_content.push_str(&escape::unescape(text_str).unwrap_or_default());
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
    }

    let re_to = Regex::new(r"(?m)^To:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    let re_from = Regex::new(r"(?m)^From:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    let re_subj = Regex::new(r"(?m)^Subject:\s*(.*(?:\r?\n\s+.*)*)").unwrap();

    let to_addr = re_to
        .captures(&mime_content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    let from_addr = re_from
        .captures(&mime_content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    let status = if let (Some(to), Some(from)) = (to_addr, from_addr) {
        let subject = re_subj
            .captures(&mime_content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();

        let body_start = mime_content.find("\r\n\r\n").unwrap_or(0);
        let (_, body_content) = mime_content.split_at(body_start);
        let clean_body = body_content.trim_start_matches("\r\n\r\n");

        let email = lettre::Message::builder()
            .from(from.parse().unwrap())
            .to(to.parse().unwrap())
            .subject(subject)
            .body(clean_body.to_string())
            .unwrap();

        let smtp_url = url::Url::parse(&config.smtp_url).unwrap();
        let host = smtp_url.host_str().unwrap();
        let port = smtp_url.port().unwrap_or(25);

        let mailer = SmtpTransport::builder_dangerous(host).port(port).build();

        let smtp_url = &config.smtp_url;

        let smtp_host = match smtp_url.host_str() {
            Some(h) => h,
            None => {
                tracing::error!("SMTP URL has no host");
                return SEND_MAIL_ERROR.to_string();
            }
        };

        let scheme = smtp_url.scheme().to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "smtps" => 465,
            "smtp" => 25,
            _ => 587,
        };
        let port = smtp_url.port().unwrap_or(default_port);

        let mut builder = if scheme == "smtps" {
            // Implicit TLS (port 465): relay() already configures TLS::Wrapper internally.
            let b = match AsyncSmtpTransport::<Tokio1Executor>::relay(smtp_host) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("Failed to create SMTP relay transport: {}", e);
                    return SEND_MAIL_ERROR.to_string();
                }
            };
            b.port(port)
        } else {
            // Plain SMTP (port 25) or STARTTLS: use builder_dangerous to allow
            // unencrypted connections, restoring compatibility with smtp:// URLs.
            let b = if scheme == "smtp" {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
            } else {
                match AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host) {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::error!("Failed to create SMTP STARTTLS transport: {}", e);
                        return SEND_MAIL_ERROR.to_string();
                    }
                }
            };
            b.port(port)
        };

        // Optional basic auth from URL: smtp://user:pass@host:port
        // URL components are percent-encoded, so decode before use as credentials.
        let user = percent_decode_str(smtp_url.username()).decode_utf8_lossy();
        if scheme == "smtp" && !user.is_empty() && smtp_url.password().is_some() {
            tracing::error!("SMTP credentials require TLS; use smtps:// or starttls://");
            return SEND_MAIL_ERROR.to_string();
        }
        if !user.is_empty()
            && let Some(pass) = smtp_url.password() {
                let pass = percent_decode_str(pass).decode_utf8_lossy();
                builder = builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
                    user.into_owned(),
                    pass.into_owned(),
                ));
            }

        let mailer = builder.build();

        match mailer.send(email).await {
            Ok(_) => "1",
            Err(e) => {
                tracing::error!("SMTP Error: {}", e);
                "2"
            }
        }
    } else {
        tracing::warn!("SendMail failed: Missing To or From header");
        "2"
    };

    format!(
        r#"<SendMail xmlns="AirSync:"><Status>{}</Status></SendMail>"#,
        status
    )
}

async fn handle_ping() -> String {
    r#"<Ping xmlns="Ping:"><Status>1</Status></Ping>"#.to_string()
}

async fn handle_settings(
    _session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
) -> String {
    db::register_device(config, user, device_id).await;
    r#"<Settings xmlns="Settings:">
        <Status>1</Status>
        <DeviceInformation>
            <Status>1</Status>
        </DeviceInformation>
    </Settings>"#
        .to_string()
}

async fn handle_folder_sync(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
) -> String {
    db::register_device(config, user, device_id).await;

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

    format!(
        r#"<FolderSync xmlns="AirSync:">
            <Status>1</Status>
            <Collections>
                <Collection>
                    <SyncKey>0</SyncKey>
                    <Changes>
                        <Count>1</Count>
                        <Add>
                            <ServerId>{}</ServerId>
                            <ParentId>0</ParentId>
                            <DisplayName>Calendar</DisplayName>
                            <Type>8</Type>
                        </Add>
                    </Changes>
                </Collection>
            </Collections>
        </FolderSync>"#,
        escape_xml(&cal_id)
    )
}

async fn handle_sync(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
    req: SyncRequest,
) -> String {
    let coll = req.collections.collection;
    let old_sync_key = coll.sync_key.clone();

    if let Some(cmds) = coll.commands {
        process_client_commands(session, cmds, &config.timezone).await;
    }

    let current_jmap_state = match jmap_client::get_calendar_state(
        &session.api_url,
        &session.access_token,
        &session.account_id,
    )
    .await
    {
        Ok(s) => s,
        Err(_) => return error_xml(500, "JMAPStateError"),
    };

    let prev_state = db::get_sync_state(config, user, device_id, &coll.collection_id).await;

    let (items_xml, new_sync_key) = if old_sync_key == "0" || prev_state.is_none() {
        let events = jmap_client::get_calendar_events(
            &session.api_url,
            &session.access_token,
            &session.account_id,
        )
        .await
        .unwrap_or_default();
        render_items(&events, "Add", &config.timezone)
    } else {
        let prev_jmap_state = prev_state.unwrap();

        if prev_jmap_state == current_jmap_state {
            (String::new(), old_sync_key.clone())
        } else {
            let changes = jmap_client::get_calendar_changes(
                &session.api_url,
                &session.access_token,
                &session.account_id,
                &prev_jmap_state,
            )
            .await
            .unwrap_or_default();
            render_changes(
                &session.api_url,
                &session.access_token,
                &session.account_id,
                changes,
                &config.timezone,
            )
            .await
        }
    };

    let final_sync_key = if new_sync_key != old_sync_key {
        db::update_sync_state(
            config,
            user,
            device_id,
            &coll.collection_id,
            &new_sync_key,
            &current_jmap_state,
        )
        .await;
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
        escape_xml(&final_sync_key),
        escape_xml(&coll.collection_id),
        items_xml
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

        let body_type = 2;
        let body_content = escape_xml(event.description.as_deref().unwrap_or(""));
        let body_size = body_content.len();

        let mut attendees_xml = String::new();
        if let Some(attendees) = &event.participants {
            for att in attendees {
                attendees_xml.push_str(&format!(
                    r#"<Attendee>
                        <Email>{}</Email>
                        <Name>{}</Name>
                        <AttendeeStatus>{}</AttendeeStatus>
                        <AttendeeType>1</AttendeeType>
                       </Attendee>"#,
                    escape_xml(&att.email),
                    escape_xml(&att.name),
                    "0"
                ));
            }
        }

        xml.push_str(&format!(
            r#"<{}><ServerId>{}</ServerId><ApplicationData>
                <Calendar:Subject>{}</Calendar:Subject>
                <Calendar:Location>{}</Calendar:Location>
                <Calendar:Start>{}</Calendar:Start>
                <Calendar:End>{}</Calendar:End>
                <Calendar:UID>{}</Calendar:UID>
                <Calendar:AllDayEvent>{}</Calendar:AllDayEvent>
                <AirSyncBase:Body>
                    <AirSyncBase:Type>{}</AirSyncBase:Type>
                    <AirSyncBase:EstimatedDataSize>{}</AirSyncBase:EstimatedDataSize>
                    <AirSyncBase:Data>{}</AirSyncBase:Data>
                </AirSyncBase:Body>
                <Calendar:Attendees>{}</Calendar:Attendees>
            </ApplicationData></{}>"#,
            mode,
            escape_xml(event.id.as_deref().unwrap_or("")),
            escape_xml(&event.title),
            escape_xml(event.location.as_deref().unwrap_or("")),
            start_local.format("%Y-%m-%dT%H:%M:%S"),
            end_local.format("%Y-%m-%dT%H:%M:%S"),
            escape_xml(event.uid.as_deref().unwrap_or("")),
            if event.is_all_day { "1" } else { "0" },
            body_type,
            body_size,
            body_content,
            attendees_xml,
            mode
        ));
    }
    (xml, new_key)
}

async fn render_changes(
    url: &str,
    token: &str,
    account_id: &str,
    changes: jmap_client::JmapChanges,
    tz_str: &str,
) -> (String, String) {
    let mut xml = String::new();
    let new_key = uuid::Uuid::new_v4().to_string();

    for id in &changes.destroyed {
        xml.push_str(&format!(
            r#"<Delete><ServerId>{}</ServerId></Delete>"#,
            escape_xml(id)
        ));
    }

    if !changes.updated.is_empty()
        && let Ok(events) =
            jmap_client::get_events_by_ids(url, token, account_id, &changes.updated).await
    {
        let (change_xml, _) = render_items(&events, "Change", tz_str);
        xml.push_str(&change_xml);
    }
    (xml, new_key)
}

async fn process_client_commands(session: &jmap_client::JmapSession, cmds: Commands, tz_str: &str) {
    let client = reqwest::Client::new();
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    for add_cmd in cmds.add.unwrap_or_default() {
        let data = add_cmd.application_data;

        let start_utc = parse_local_to_utc(&data.start.unwrap_or_default(), tz);
        let end_utc = parse_local_to_utc(&data.end.unwrap_or_default(), tz);

        let attendees: Vec<jmap_client::Participant> = data
            .attendees
            .map(|a| {
                a.items
                    .into_iter()
                    .map(|att| jmap_client::Participant {
                        email: att.email,
                        name: att.name,
                        status: None, // Fix for E0063
                    })
                    .collect()
            })
            .unwrap_or_default();

        let event = jmap_client::JmapEvent {
            id: None,
            title: data.subject.unwrap_or_default(),
            start: start_utc,
            end: end_utc,
            location: data.location,
            description: data.body.map(|b| b.data),
            uid: data.uid,
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            is_all_day: data.all_day_event.unwrap_or(0) == 1,
        };
        let _ = jmap_client::push_event(
            &session.api_url,
            &session.access_token,
            &session.account_id,
            event,
        )
        .await;
    }

    for change_cmd in cmds.change.unwrap_or_default() {
        let id = change_cmd.server_id;
        let data = change_cmd.application_data;

        let mut patch = serde_json::Map::new();

        if let Some(s) = data.subject {
            patch.insert("title".to_string(), serde_json::json!(s));
        }
        if let Some(l) = data.location {
            patch.insert("location".to_string(), serde_json::json!(l));
        }
        if let Some(s) = data.start {
            patch.insert(
                "start".to_string(),
                serde_json::json!(parse_local_to_utc(&s, tz)),
            );
        }
        if let Some(e) = data.end {
            patch.insert(
                "end".to_string(),
                serde_json::json!(parse_local_to_utc(&e, tz)),
            );
        }
        if !patch.is_empty()
            && let Err(e) = jmap_client::patch_event(session, &id, patch).await {
                tracing::error!("ActiveSync Update failed for id {}: {}", id, e);
                failures.push(CommandFailure::Change {
                    server_id: id.clone(),
                });
            }
    }

    if let Some(deletes) = cmds.delete
        && !deletes.is_empty()
    {
        let ids: Vec<String> = deletes.into_iter().map(|d| d.server_id).collect();

        let body = serde_json::json!({
            "using": ["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:calendars"],
            "methodCalls": [
                ["CalendarEvent/set", {
                    "accountId": session.account_id,
                    "destroy": ids
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
            tracing::error!("ActiveSync Delete failed: {}", e);
        }
    }
}

fn parse_local_to_utc(local_str: &str, tz: Tz) -> String {
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(local_str, "%Y-%m-%dT%H:%M:%S") {
        return tz
            .from_local_datetime(&dt)
            .single()
            .map(|dt| dt.with_timezone(&Utc).to_rfc3339())
            .unwrap_or_default();
    }
    local_str.to_string()
}

async fn handle_meeting_response(
    session: &jmap_client::JmapSession,
    _config: &AppConfig,
    xml: &str,
    user_email: &str,
) -> String {
    let mut uid = String::new();
    let mut response_code = 0;
    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                current_tag = std::str::from_utf8(e.local_name().as_ref())
                    .unwrap_or("")
                    .to_string()
            }
            Ok(Event::Text(t)) => {
                let text_str = std::str::from_utf8(&t).unwrap_or("");
                let text = escape::unescape(text_str).unwrap_or_default();
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

    let event_id = match jmap_client::find_event_by_uid(
        &session.api_url,
        &session.access_token,
        &session.account_id,
        &uid,
    )
    .await
    {
        Ok(id) => id,
        Err(_) => return error_xml(400, "EventNotFound"),
    };

    let status_str = match response_code {
        1 => "accepted",
        2 => "tentative",
        3 => "declined",
        _ => "needs-action",
    };

    let _ = jmap_client::update_participant_status(
        &session.api_url,
        &session.access_token,
        &session.account_id,
        &event_id,
        user_email,
        status_str,
    )
    .await;

    format!(
        r#"<MeetingResponse xmlns="AirSync:">
            <Result>
                <Status>1</Status>
                <CalendarId>{}</CalendarId>
            </Result>
        </MeetingResponse>"#,
        escape_xml(&event_id)
    )
}

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn error_xml(code: i32, msg: &str) -> String {
    format!(
        "<Error><Code>{}</Code><Message>{}</Message></Error>",
        code, msg
    )
}
