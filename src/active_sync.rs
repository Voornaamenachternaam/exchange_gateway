use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use lettre::message::Message;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use quick_xml::Reader;
use quick_xml::escape;
use quick_xml::events::Event;
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const SEND_MAIL_ERROR: &str = r#"<SendMail xmlns="AirSync:"><Status>2</Status></SendMail>"#;

#[derive(Debug, Serialize, Deserialize, Default)]
struct Recurrence {
    #[serde(rename = "Type")]
    r#type: i32,
    #[serde(rename = "Interval")]
    interval: i32,
    #[serde(rename = "DayOfWeek", skip_serializing_if = "Option::is_none")]
    day_of_week: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct ApplicationData {
    #[serde(rename = "Subject", default)]
    subject: Option<String>,
    #[serde(rename = "Location", default)]
    location: Option<String>,
    #[serde(rename = "StartTime", default)]
    start: Option<String>,
    #[serde(rename = "EndTime", default)]
    end: Option<String>,
    #[serde(rename = "Body", default)]
    body: Option<BodyData>,
    #[serde(rename = "UID", default)]
    uid: Option<String>,
    #[serde(rename = "Attendees", default)]
    attendees: Option<AttendeesList>,
    #[serde(rename = "AllDayEvent", default)]
    all_day_event: Option<i32>,
    #[serde(rename = "Recurrence", default)]
    recurrence: Option<Recurrence>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BodyData {
    #[serde(rename = "Type")]
    body_type: i32,
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
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut command = String::new();
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
                    command = clean_name.to_string();
                    break;
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
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
        "Sync" => {
            let req: SyncRequest = match quick_xml::de::from_str(xml) {
                Ok(r) => r,
                Err(e) => {
                    tracing::error!("Sync XML Parse Error: {:?}", e);
                    return error_xml(400, "BadRequest");
                }
            };
            handle_sync(&session, config, &user, device_id, req).await
        }
        "ItemOperations" => {
            let req: ItemOpsReq = match quick_xml::de::from_str(xml) {
                Ok(r) => r,
                Err(_) => return error_xml(400, "BadRequest"),
            };
            handle_item_operations(&session, req).await
        }
        "MeetingResponse" => handle_meeting_response(&session, xml, &user).await,
        "SendMail" => handle_send_mail(config, xml, &user).await,
        "Settings" => handle_settings(&session, config, &user, device_id).await,
        "Provision" => handle_provision().await,
        "Search" => handle_search(&session, xml).await,
        "Ping" => handle_ping().await,
        _ => {
            tracing::warn!("Unsupported EAS Command: {}", command);
            error_xml(400, "UnsupportedCommand")
        }
    }
}

async fn handle_send_mail(config: &AppConfig, xml: &str, authenticated_user: &str) -> String {
    let mut mime_content = String::new();
    let mut buf = Vec::new();
    let mut in_mime = false;
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                if String::from_utf8_lossy(e.local_name().as_ref()) == "Mime" {
                    in_mime = true;
                }
            }
            Ok(Event::End(ref e)) => {
                if String::from_utf8_lossy(e.local_name().as_ref()) == "Mime" {
                    in_mime = false;
                }
            }
            Ok(Event::Text(t)) => {
                if in_mime {
                    mime_content.push_str(
                        &escape::unescape(std::str::from_utf8(&t).unwrap_or(""))
                            .unwrap_or_default(),
                    );
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    if mime_content.is_empty() {
        return SEND_MAIL_ERROR.to_string();
    }

    let re_to = Regex::new(r"(?m)^To:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    let re_from = Regex::new(r"(?m)^From:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    let re_subj = Regex::new(r"(?m)^Subject:\s*(.*(?:\r?\n\s+.*)*)").unwrap();

    let to_addr = re_to
    lazy_static! {
        static ref RE_TO: Regex = Regex::new(r"(?m)^To:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
        static ref RE_FROM: Regex = Regex::new(r"(?m)^From:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
        static ref RE_SUBJ: Regex = Regex::new(r"(?m)^Subject:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    }
    let from_addr = re_from
        .captures(&mime_content)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    // Verify the From address matches the authenticated user to prevent spoofing
    let from_addr = match from_addr {
        Some(f) => f,
        None => {
            tracing::warn!("SendMail: Missing From header");
            return SEND_MAIL_ERROR.to_string();
        }
    };
    let from_email = match from_addr.parse::<lettre::message::Mailbox>() {
        Ok(mb) => mb.email.to_string(),
        Err(e) => {
            tracing::warn!("SendMail: Malformed From header '{}': {}", from_addr, e);
            return SEND_MAIL_ERROR.to_string();
        }
    };
    // Compare the from address against the authenticated user. The auth username
    // may be a full email (user@domain) or just the local part (user). Accept
    // the message if the full email matches, or if the auth username has no '@'
    // and matches the local part of the from address AND the domain matches the
    // configured mail domain (to prevent domain spoofing).
    let from_matches = if authenticated_user.contains('@') {
        from_email.eq_ignore_ascii_case(authenticated_user)
    } else {
        let local_matches = from_email
            .split('@')
            .next()
            .unwrap_or("")
            .eq_ignore_ascii_case(authenticated_user);
        let domain_matches = from_email
            .split('@')
            .nth(1)
            .unwrap_or("")
            .eq_ignore_ascii_case(&config.mail_domain);
        local_matches && domain_matches
    };
    if !from_matches {
        tracing::warn!(
            "SendMail: From address '{}' does not match authenticated user",
            from_email
        );
        return SEND_MAIL_ERROR.to_string();
    }
    let from_addr = Some(from_email);

    let status = if let (Some(to), Some(from)) = (to_addr, from_addr) {
        let subject = re_subj
            .captures(&mime_content)
            .and_then(|c| c.get(1))
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        let body_start = mime_content.find("\r\n\r\n").unwrap_or(0);
        let clean_body = mime_content
            .split_at(body_start)
            .1
            .trim_start_matches("\r\n\r\n");

        let email = match (from.parse(), to.parse()) {
            (Ok(f), Ok(t)) => match Message::builder()
                .from(f)
                .to(t)
                .subject(subject)
                .body(clean_body.to_string())
            {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("Email build error: {}", e);
                    return SEND_MAIL_ERROR.to_string();
                }
            },
            _ => {
                tracing::warn!("SendMail: invalid From/To address");
                return SEND_MAIL_ERROR.to_string();
            }
        };

        let smtp_url = match url::Url::parse(&config.smtp_url) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("Invalid SMTP URL: {}", e);
                return SEND_MAIL_ERROR.to_string();
            }
        };

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
            // Implicit TLS (port 465): use relay() which enforces TLS, then wrap.
            let b = match AsyncSmtpTransport::<Tokio1Executor>::relay(smtp_host) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("Failed to create SMTP relay transport: {}", e);
                    return SEND_MAIL_ERROR.to_string();
                }
            };
            let tls = match lettre::transport::smtp::client::TlsParameters::new(smtp_host.to_string()) {
                Ok(t) => t,
                Err(e) => {
                    tracing::error!("Failed to configure SMTPS TLS parameters: {}", e);
                    return SEND_MAIL_ERROR.to_string();
                }
            };
            b.port(port).tls(lettre::transport::smtp::client::Tls::Wrapper(tls))
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
        let user = smtp_url.username();
        if !user.is_empty() {
            if let Some(pass) = smtp_url.password() {
                builder = builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
                    user.to_string(),
                    pass.to_string(),
                ));
            }
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

async fn handle_meeting_response(
    session: &jmap_client::JmapSession,
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
                current_tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
            }
            Ok(Event::Text(t)) => {
                let text =
                    escape::unescape(std::str::from_utf8(&t).unwrap_or("")).unwrap_or_default();
                match current_tag.as_str() {
                    "RequestId" => uid = text.to_string(),
                    "UserResponse" => response_code = text.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    if uid.is_empty() {
        return error_xml(400, "Missing UID");
    }

    let event_id = match jmap_client::find_event_by_uid(session, &uid).await {
        Ok(id) => id,
        Err(_) => return error_xml(400, "EventNotFound"),
    };
    let status_str = match response_code {
        1 => "accepted",
        2 => "tentative",
        3 => "declined",
        _ => "needs-action",
    };
    let _ = jmap_client::update_participant_status(session, &event_id, user_email, status_str)
        .await;

    format!(
        r#"<MeetingResponse xmlns="AirSync:"><Result><Status>1</Status><CalendarId>{}</CalendarId></Result></MeetingResponse>"#,
        utils::escape_xml(&event_id)
    )
}

async fn handle_search(session: &jmap_client::JmapSession, xml: &str) -> String {
    let mut query = String::new();
    let mut buf = Vec::new();
    let mut current_tag = String::new();
    let mut reader = Reader::from_str(xml);

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                current_tag = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
            }
            Ok(Event::Text(t)) => {
                if current_tag == "FreeText" {
                    query = escape::unescape(std::str::from_utf8(&t).unwrap_or(""))
                        .unwrap_or_default()
                        .to_string();
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    let results = jmap_client::search_principals(session, &query)
        .await
        .unwrap_or_default();
    let results: Vec<_> = results.into_iter().take(10).collect();
    let mut results_xml = String::new();
    for p in results {
        results_xml.push_str(&format!(r#"<Result><Properties><DisplayName>{}</DisplayName><EmailAddress>{}</EmailAddress></Properties></Result>"#, utils::escape_xml(&p.name), utils::escape_xml(&p.email)));
    }
    format!(
        r#"<Search xmlns="Search:"><Status>1</Status><Response><Store><Status>1</Status><Result>{}</Result></Store></Response></Search>"#,
        results_xml
    )
}

async fn handle_item_operations(session: &jmap_client::JmapSession, req: ItemOpsReq) -> String {
    let mut fetches = String::new();
    for fetch in req.fetch.into_iter() {
        if let Some(store) = fetch.store {
            let id_opt = store.server_id.or(store.file_reference);
            if let Some(id) = id_opt {
                if let Ok(event) = jmap_client::get_event_by_id(session, &id).await {
                    fetches.push_str(&format!(r#"<Fetch><Status>1</Status><ServerId>{}</ServerId><ApplicationData><AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>{}</AirSyncBase:Data></AirSyncBase:Body></ApplicationData></Fetch>"#, utils::escape_xml(&id), utils::escape_xml(event.description.as_deref().unwrap_or(""))));
                } else {
                    fetches.push_str("<Fetch><Status>6</Status></Fetch>");
                }
            }
        }
    }
    format!(
        r#"<ItemOperations xmlns="ItemOperations:" xmlns:AirSyncBase="AirSyncBase:"><Status>1</Status><Response>{}</Response></ItemOperations>"#,
        fetches
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
    let collection_id = coll.collection_id.clone();

    if let Some(cmds) = coll.commands {
        process_client_commands(session, cmds, &config.timezone, user).await;
    }

    let current_jmap_state = match jmap_client::get_calendar_state(session).await {
        Ok(s) => s,
        Err(_) => return error_xml(500, "JMAPStateError"),
    };
    let prev_state = db::get_sync_state(config, user, device_id, &collection_id).await;

    // Fix: Use match to avoid unwrap and handle logic clearly
    let (items_xml, new_sync_key) = match prev_state {
        Some(prev_jmap_state) if old_sync_key != "0" => {
            if prev_jmap_state == current_jmap_state {
                // Keep schema consistent with the normal response path by always including <Commands>.
                return format!(
                    r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands></Commands></Collection></Collections></Sync>"#,
                    utils::escape_xml(&old_sync_key),
                    utils::escape_xml(&collection_id)
                );
            }
            let changes = match jmap_client::get_calendar_changes(session, &prev_jmap_state).await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Failed to fetch calendar changes: {}", e);
                    return error_xml(500, "CalendarChangesError");
                }
            };
            match render_changes(session, changes, &config.timezone).await {
                Ok(result) => result,
                Err(e) => {
                    tracing::error!("Failed to render calendar changes: {}", e);
                    return error_xml(500, "CalendarChangesError");
                }
            }
        }
        _ => {
            // Either prev_state is None, or old_sync_key is "0" (client reset)
            let events = match jmap_client::get_calendar_events(session).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("Failed to fetch calendar events: {}", e);
                    return error_xml(500, "CalendarEventsError");
                }
            };
            let mut xml = String::new();
            let new_key = Uuid::new_v4().to_string();
            for event in events {
                xml.push_str(&render_event_xml(event, "Add", &config.timezone));
            }
            (xml, new_key)
        }
    };

    if new_sync_key != old_sync_key {
        db::update_sync_state(
            config,
            user,
            device_id,
            &collection_id,
            &new_sync_key,
            &current_jmap_state,
        )
        .await;
    }

    format!(
        r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands></Collection></Collections></Sync>"#,
        utils::escape_xml(&new_sync_key),
        utils::escape_xml(&collection_id),
        items_xml
    )
}

fn render_event_xml(event: jmap_client::JmapEvent, mode: &str, tz_str: &str) -> String {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    // Avoid silently defaulting to Unix epoch on parse errors.
    // If parsing fails, log and fall back to using the original JMAP string as-is.
    let start_str = match DateTime::parse_from_rfc3339(&event.start)
        .map(|dt| dt.with_timezone(&Utc))
        .map(|dt| dt.with_timezone(&tz))
    {
        Ok(start_local) => start_local.format("%Y-%m-%dT%H:%M:%S").to_string(),
        Err(e) => {
            tracing::warn!(
                "Invalid JMAP event start timestamp for event id {:?}: '{}' ({})",
                event.id,
                event.start,
                e
            );
            event.start.clone()
        }
    };

    let end_str = match DateTime::parse_from_rfc3339(&event.end)
        .map(|dt| dt.with_timezone(&Utc))
        .map(|dt| dt.with_timezone(&tz))
    {
        Ok(end_local) => end_local.format("%Y-%m-%dT%H:%M:%S").to_string(),
        Err(e) => {
            tracing::warn!(
                "Invalid JMAP event end timestamp for event id {:?}: '{}' ({})",
                event.id,
                event.end,
                e
            );
            event.end.clone()
        }
    };

    let mut attendees_xml = String::new();
    if let Some(attendees) = &event.participants {
        attendees_xml.push_str("<Calendar:Attendees>");
        for att in attendees {
            attendees_xml.push_str(&format!(r#"<Calendar:Attendee><Calendar:Email>{}</Calendar:Email><Calendar:Name>{}</Calendar:Name></Calendar:Attendee>"#, utils::escape_xml(&att.email), utils::escape_xml(&att.name)));
        }
        attendees_xml.push_str("</Calendar:Attendees>");
    }
    let recurrence_xml = if let Some(rrule) = &event.recurrence_rule {
        parse_rrule_to_eas(rrule)
    } else {
        String::new()
    };
    let body_content = utils::escape_xml(event.description.as_deref().unwrap_or(""));

    format!(
        r#"<{}><ServerId>{}</ServerId><ApplicationData><Calendar:Subject>{}</Calendar:Subject><Calendar:Location>{}</Calendar:Location><Calendar:StartTime>{}</Calendar:StartTime><Calendar:EndTime>{}</Calendar:EndTime><Calendar:UID>{}</Calendar:UID><Calendar:AllDayEvent>{}</Calendar:AllDayEvent>{}{}<AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>{}</AirSyncBase:Data></AirSyncBase:Body></ApplicationData></{}>"#,
        mode,
        utils::escape_xml(event.id.as_deref().unwrap_or("")),
        utils::escape_xml(&event.title),
        utils::escape_xml(event.location.as_deref().unwrap_or("")),
        utils::escape_xml(&start_str),
        utils::escape_xml(&end_str),
        utils::escape_xml(event.uid.as_deref().unwrap_or("")),
        if event.is_all_day { "1" } else { "0" },
        recurrence_xml,
        attendees_xml,
        body_content,
        mode
    )
}

fn parse_rrule_to_eas(rrule: &str) -> String {
    let parts: Vec<&str> = rrule.split(';').collect();
    let mut freq = "0";
    let mut interval = "1";
    let mut day_of_week = String::new();
    for part in parts {
        // Fix: Use strip_prefix
        if let Some(val) = part.strip_prefix("FREQ=") {
            freq = match val {
                "DAILY" => "0",
                "WEEKLY" => "1",
                "MONTHLY" => "2",
                "YEARLY" => "3",
                _ => "0",
            };
        }
        if let Some(val) = part.strip_prefix("INTERVAL=") {
            if val.parse::<u32>().is_ok() {
                interval = val;
            }
        }
        if let Some(val) = part.strip_prefix("BYDAY=") {
            day_of_week = val
                .split(',')
                .map(|d| match d.trim() {
                    "MO" => "2",
                    "TU" => "4",
                    "WE" => "8",
                    "TH" => "16",
                    "FR" => "32",
                    "SA" => "64",
                    "SU" => "1",
                    _ => "",
                })
                .filter_map(|s| s.parse::<i32>().ok())
                .fold(0i32, |acc, v| acc | v)
                .to_string();
        }
    }
    let day_xml = if day_of_week.is_empty() {
        String::new()
    } else {
        format!("<Calendar:DayOfWeek>{}</Calendar:DayOfWeek>", day_of_week)
    };
    format!(
        r#"<Calendar:Recurrence><Calendar:Type>{}</Calendar:Type><Calendar:Interval>{}</Calendar:Interval>{}</Calendar:Recurrence>"#,
        freq, interval, day_xml
    )
}

async fn process_client_commands(
    session: &jmap_client::JmapSession,
    cmds: Commands,
    tz_str: &str,
    _user: &str,
) {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let cal_id = jmap_client::get_default_calendar_id(session)
        .await
        .unwrap_or("default".into());
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
                        status: None,
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
            uid: data.uid.or(Some(Uuid::new_v4().to_string())),
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            is_all_day: data.all_day_event.unwrap_or(0) == 1,
            recurrence_rule: data.recurrence.map(build_rrule),
            updated: None,
        };
        let _ = jmap_client::push_event(session, event, &cal_id).await;
    }
    for change_cmd in cmds.change.unwrap_or_default() {
        let id = change_cmd.server_id;
        let data = change_cmd.application_data;
        let mut patch = serde_json::Map::new();
        if let Some(s) = data.subject {
            patch.insert("title".into(), serde_json::json!(s));
        }
        if let Some(l) = data.location {
            patch.insert("location".into(), serde_json::json!(l));
        }
        if let Some(s) = data.start {
            patch.insert(
                "start".into(),
                serde_json::json!(parse_local_to_utc(&s, tz)),
            );
        }
        if let Some(e) = data.end {
            patch.insert("end".into(), serde_json::json!(parse_local_to_utc(&e, tz)));
        }
        if let Some(b) = data.body {
            patch.insert("description".into(), serde_json::json!(b.data));
        }
        if !patch.is_empty() {
            let _ = jmap_client::patch_event(session, &id, patch).await;
        }
    }
    if let Some(deletes) = cmds.delete
        && !deletes.is_empty()
    {
        let ids: Vec<String> = deletes.into_iter().map(|d| d.server_id).collect();
        let _ = jmap_client::destroy_events(session, ids).await;
    }
}

fn build_rrule(r: Recurrence) -> String {
    let freq = match r.r#type {
        0 => "DAILY",
        1 => "WEEKLY",
        2 => "MONTHLY",
        3 => "YEARLY",
        _ => "DAILY",
    };
    let mut parts = vec![format!("FREQ={}", freq)];
    parts.push(format!("INTERVAL={}", r.interval));
    if let Some(dow) = r.day_of_week {
        let mut days = Vec::new();
        if (dow & 1) != 0 {
            days.push("SU");
        }
        if (dow & 2) != 0 {
            days.push("MO");
        }
        if (dow & 4) != 0 {
            days.push("TU");
        }
        if (dow & 8) != 0 {
            days.push("WE");
        }
        if (dow & 16) != 0 {
            days.push("TH");
        }
        if (dow & 32) != 0 {
            days.push("FR");
        }
        if (dow & 64) != 0 {
            days.push("SA");
        }
        if !days.is_empty() {
            parts.push(format!("BYDAY={}", days.join(",")));
        }
    }
    parts.join(";")
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

async fn render_changes(
    session: &jmap_client::JmapSession,
    changes: jmap_client::JmapChanges,
    tz_str: &str,
) -> Result<(String, String), String> {
    let mut xml = String::new();
    let new_key = Uuid::new_v4().to_string();
    for id in &changes.destroyed {
        xml.push_str(&format!(
            "<Delete><ServerId>{}</ServerId></Delete>",
            utils::escape_xml(id)
        ));
    }
    if !changes.created.is_empty() {
        let events = jmap_client::get_events_by_ids(session, &changes.created).await?;
        for event in events {
            xml.push_str(&render_event_xml(event, "Add", tz_str));
        }
    }
    if !changes.updated.is_empty() {
        let events = jmap_client::get_events_by_ids(session, &changes.updated).await?;
        for event in events {
            xml.push_str(&render_event_xml(event, "Change", tz_str));
        }
    }
    Ok((xml, new_key))
}

fn error_xml(code: i32, msg: &str) -> String {
    format!(
        "<Error><Code>{}</Code><Message>{}</Message></Error>",
        code, msg
    )
}

#[derive(Debug, Deserialize)]
struct SyncRequest {
    #[serde(rename = "Collections")]
    collections: SyncCollections,
}
#[derive(Debug, Deserialize)]
struct SyncCollections {
    #[serde(rename = "Collection")]
    collection: SyncCollection,
}
#[derive(Debug, Deserialize)]
struct SyncCollection {
    #[serde(rename = "SyncKey")]
    sync_key: String,
    #[serde(rename = "CollectionId")]
    collection_id: String,
    #[serde(rename = "Commands", skip_serializing_if = "Option::is_none")]
    commands: Option<Commands>,
}
#[derive(Debug, Deserialize)]
struct Commands {
    #[serde(rename = "Add", skip_serializing_if = "Option::is_none")]
    add: Option<Vec<AddCommand>>,
    #[serde(rename = "Change", skip_serializing_if = "Option::is_none")]
    change: Option<Vec<ChangeCommand>>,
    #[serde(rename = "Delete", skip_serializing_if = "Option::is_none")]
    delete: Option<Vec<DeleteCommand>>,
}
#[derive(Debug, Deserialize)]
struct AddCommand {
    #[serde(rename = "ClientId")]
    _client_id: String,
    #[serde(rename = "ApplicationData")]
    application_data: ApplicationData,
}
#[derive(Debug, Deserialize)]
struct ChangeCommand {
    #[serde(rename = "ServerId")]
    server_id: String,
    #[serde(rename = "ApplicationData")]
    application_data: ApplicationData,
}
#[derive(Debug, Deserialize)]
struct DeleteCommand {
    #[serde(rename = "ServerId")]
    server_id: String,
}
#[derive(Debug, Deserialize)]
struct ItemOpsReq {
    #[serde(rename = "Fetch")]
    fetch: Vec<ItemOpsFetch>,
}
#[derive(Debug, Deserialize)]
struct ItemOpsFetch {
    #[serde(rename = "Store")]
    store: Option<ItemOpsStore>,
}
#[derive(Debug, Deserialize)]
struct ItemOpsStore {
    #[serde(rename = "ServerId", default)]
    server_id: Option<String>,
    #[serde(rename = "FileReference", default)]
    file_reference: Option<String>,
}

async fn handle_provision() -> String {
    r#"<Provision xmlns="Provision:"><Status>1</Status><Policies><Policy><PolicyType>MS-EAS-Provisioning-WBXML</PolicyType><Status>1</Status><PolicyKey>12345</PolicyKey></Policy></Policies></Provision>"#.into()
}
async fn handle_settings(
    _session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
) -> String {
    db::register_device(config, user, device_id).await;
    r#"<Settings xmlns="Settings:"><Status>1</Status></Settings>"#.into()
}
async fn handle_ping() -> String {
    r#"<Ping xmlns="Ping:"><Status>1</Status></Ping>"#.into()
}
async fn handle_folder_sync(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
) -> String {
    db::register_device(config, user, device_id).await;
async fn process_client_commands(
    session: &jmap_client::JmapSession,
    cmds: Commands,
    tz_str: &str,
    _user: &str,
) {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to determine default calendar id; dropping client commands: {e}");
            return;
        }
    };

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
                        status: None,
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
            uid: data.uid.or(Some(Uuid::new_v4().to_string())),
            participants: if attendees.is_empty() {
                None
            } else {
                Some(attendees)
            },
            is_all_day: data.all_day_event.unwrap_or(0) == 1,
            recurrence_rule: data.recurrence.map(build_rrule),
            updated: None,
        };
        let _ = jmap_client::push_event(session, event, &cal_id).await;
    }
    for change_cmd in cmds.change.unwrap_or_default() {
        let id = change_cmd.server_id;
        let data = change_cmd.application_data;
        let mut patch = serde_json::Map::new();
        if let Some(s) = data.subject {
            patch.insert("title".into(), serde_json::json!(s));
        }
        if let Some(l) = data.location {
            patch.insert("location".into(), serde_json::json!(l));
        }
        if let Some(s) = data.start {
            patch.insert(
                "start".into(),
                serde_json::json!(parse_local_to_utc(&s, tz)),
            );
        }
        if let Some(e) = data.end {
            patch.insert("end".into(), serde_json::json!(parse_local_to_utc(&e, tz)));
        }
        if let Some(b) = data.body {
            patch.insert("description".into(), serde_json::json!(b.data));
        }
        if !patch.is_empty() {
            let _ = jmap_client::patch_event(session, &id, patch).await;
        }
    }
    if let Some(deletes) = cmds.delete
        && !deletes.is_empty()
    {
        let ids: Vec<String> = deletes.into_iter().map(|d| d.server_id).collect();
        let _ = jmap_client::destroy_events(session, ids).await;
    }
}

async fn handle_folder_sync(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
) -> String {
    db::register_device(config, user, device_id).await;

    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("FolderSync failed: unable to determine default calendar id: {e}");
            return error_xml(500, "NoCalendars");
        }
    };

    format!(
        r#"<FolderSync xmlns="AirSync:"><Status>1</Status><Collections><Collection><SyncKey>0</SyncKey><Changes><Add><ServerId>{}</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add></Changes></Collection></Collections></FolderSync>"#,
        utils::escape_xml(&cal_id)
    )
}
