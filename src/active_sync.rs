use crate::{config::AppConfig, db, jmap_client, utils};
use axum::http::HeaderMap;
use base64::Engine;
use chrono::{DateTime, NaiveDateTime};
use chrono_tz::Tz;
use lazy_static::lazy_static;
use lettre::address::Envelope;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use percent_encoding::percent_decode_str;
use quick_xml::Reader;
use quick_xml::escape;
use quick_xml::events::Event;
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

lazy_static! {
    static ref RE_TO: Regex = Regex::new(r"(?im)^to:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
    static ref RE_FROM: Regex = Regex::new(r"(?im)^from:\s*(.*(?:\r?\n\s+.*)*)").unwrap();
}

const SEND_MAIL_ERROR: &str = r#"<SendMail xmlns="ComposeMail:"><Status>2</Status></SendMail>"#;

/// Per-command failure reported back in the `<Responses>` element of the Sync
/// response so the client knows which commands failed and can retry them.
#[derive(Debug)]
enum CommandFailure {
    /// An Add command failed.  `client_id` echoes the device-assigned ClientId.
    Add { client_id: String },
    /// A Change (update) command failed.  `server_id` identifies the item.
    Change { server_id: String },
    /// A Delete command failed.  `server_id` identifies the item.
    Delete { server_id: String },
}

/// A successfully applied Add command.  The `<Responses>` element must echo
/// the ClientId back together with the server-assigned ServerId so the device
/// can correlate local and remote items.
#[derive(Debug)]
struct AddSuccess {
    client_id: String,
    server_id: String,
}

/// Bundles the context needed by [`handle_sync_change_error`] so the helper
/// stays under the recommended argument limit.
struct SyncErrorContext<'a> {
    config: &'a AppConfig,
    user: &'a str,
    device_id: &'a str,
    collection_id: &'a str,
    old_sync_key: &'a str,
    prev_jmap_state: &'a str,
    has_client_commands: bool,
    responses_xml: &'a str,
}

/// Result of processing client commands — failures and successful Add mappings.
#[derive(Debug, Default)]
struct CommandResults {
    failures: Vec<CommandFailure>,
    add_successes: Vec<AddSuccess>,
}

impl CommandResults {
    /// Render the ActiveSync `<Responses>` XML fragment.
    ///
    /// Status 1 = success, Status 6 = "Server error / object not found".
    /// Successful Add commands include a ClientId→ServerId mapping so the
    /// device can correlate local and remote items.
    fn to_responses_xml(&self) -> String {
        if self.failures.is_empty() && self.add_successes.is_empty() {
            return String::new();
        }
        let mut xml = String::from("<Responses>");
        for s in &self.add_successes {
            xml.push_str(&format!(
                "<Add><ClientId>{}</ClientId><ServerId>{}</ServerId><Status>1</Status></Add>",
                utils::escape_xml(&s.client_id),
                utils::escape_xml(&s.server_id),
            ));
        }
        for f in &self.failures {
            match f {
                CommandFailure::Add { client_id } => {
                    xml.push_str(&format!(
                        "<Add><ClientId>{}</ClientId><Status>6</Status></Add>",
                        utils::escape_xml(client_id),
                    ));
                }
                CommandFailure::Change { server_id } => {
                    xml.push_str(&format!(
                        "<Change><ServerId>{}</ServerId><Status>6</Status></Change>",
                        utils::escape_xml(server_id),
                    ));
                }
                CommandFailure::Delete { server_id } => {
                    xml.push_str(&format!(
                        "<Delete><ServerId>{}</ServerId><Status>6</Status></Delete>",
                        utils::escape_xml(server_id),
                    ));
                }
            }
        }
        xml.push_str("</Responses>");
        xml
    }
}

#[derive(Debug, Serialize, Deserialize, Default)]
struct Recurrence {
    #[serde(rename = "Type")]
    r#type: i32,
    #[serde(rename = "Interval", default = "default_interval")]
    interval: i32,
    #[serde(rename = "DayOfWeek", skip_serializing_if = "Option::is_none")]
    day_of_week: Option<i32>,
    #[serde(
        rename = "DayOfMonth",
        skip_serializing_if = "Option::is_none",
        default
    )]
    day_of_month: Option<i32>,
    #[serde(
        rename = "WeekOfMonth",
        skip_serializing_if = "Option::is_none",
        default
    )]
    week_of_month: Option<i32>,
    #[serde(
        rename = "MonthOfYear",
        skip_serializing_if = "Option::is_none",
        default
    )]
    month_of_year: Option<i32>,
}

fn default_interval() -> i32 {
    1
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

pub async fn process_request(
    config: &AppConfig,
    xml: &str,
    headers: &HeaderMap,
    query_cmd: &str,
) -> String {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut command = String::new();
    let mut buf = Vec::new();

    loop {
        buf.clear();
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                command = String::from_utf8_lossy(e.local_name().as_ref()).to_string();
                break;
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::error!("Failed to read EAS XML: {:?}", e);
                return error_xml(400, "BadRequest");
            }
            _ => {}
        }
    }

    // When the XML body is empty (no root element), fall back to the Cmd
    // query parameter from the URL.  ActiveSync clients may legitimately
    // send no body for commands like Ping and Provision.
    if command.is_empty() && !query_cmd.is_empty() {
        command = query_cmd.to_string();
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

    // Commands that do not require a JMAP session — handle them after
    // credentials have been fully validated via the JMAP session above.
    match command.as_str() {
        "Ping" => return handle_ping().await,
        "Provision" => return handle_provision().await,
        _ => {}
    }

    match command.as_str() {
        "FolderSync" => handle_folder_sync(&session, config, &user, device_id, xml).await,
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
                Err(e) => {
                    tracing::error!("ItemOperations XML Parse Error: {:?}", e);
                    return error_xml(400, "BadRequest");
                }
            };
            handle_item_operations(&session, req).await
        }
        "MeetingResponse" => handle_meeting_response(&session, config, xml, &user).await,
        "SendMail" => handle_send_mail(config, xml, &user).await,
        "Settings" => handle_settings(&session, config, &user, device_id).await,
        "Search" => handle_search(&session, xml).await,
        _ => {
            tracing::warn!("Unsupported EAS Command: {}", command);
            error_xml(400, "UnsupportedCommand")
        }
    }
}

async fn handle_send_mail(config: &AppConfig, xml: &str, authenticated_user: &str) -> String {
    let mut mime_text = String::new();
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
                    let text = String::from_utf8_lossy(&t);
                    let unescaped = match escape::unescape(&text) {
                        Ok(s) => s.into_owned(),
                        Err(e) => {
                            tracing::warn!("SendMail: failed to unescape XML text: {:?}", e);
                            text.into_owned()
                        }
                    };
                    mime_text.push_str(&unescaped);
                }
            }
            Ok(Event::CData(t)) => {
                if in_mime {
                    mime_text.push_str(&String::from_utf8_lossy(t.as_ref()));
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    // WBXML opaque data is base64-encoded by wbxml::decode. Only attempt
    // to decode when the content does not already look like raw MIME text,
    // so that plain-text MIME arriving via XML is not accidentally corrupted.
    // We detect raw MIME by looking for RFC 822-style header lines
    // (e.g. "Header-Name: value") anywhere in the first 10 lines. This
    // covers messages that start with Received:, Date:, Subject:, etc.
    let looks_like_mime = mime_text.lines().take(10).any(|l| {
        let Some((name, _)) = l.split_once(':') else {
            return false;
        };
        // RFC 5322 field names: printable ASCII except colon, at least 1 char
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_graphic() && b != b':')
    });
    // Use Vec<u8> for the final MIME payload to avoid corrupting non-UTF-8
    // bytes (e.g. binary attachments) via lossy UTF-8 conversion.
    let mime_content: Vec<u8> = if !looks_like_mime {
        let stripped: String = mime_text
            .chars()
            .filter(|c| !c.is_ascii_whitespace())
            .collect();
        match base64::engine::general_purpose::STANDARD.decode(&stripped) {
            Ok(decoded_bytes) => decoded_bytes,
            Err(e) => {
                tracing::warn!("SendMail: base64 decode failed, using raw text: {e}");
                mime_text.into_bytes()
            }
        }
    } else {
        mime_text.into_bytes()
    };

    if mime_content.is_empty() {
        return SEND_MAIL_ERROR.to_string();
    }

    // Extract the RFC 5322 header section for To/From parsing.  Headers are
    // always ASCII, so we only need the bytes up to the first blank line
    // (the header/body separator).  Using lossy conversion here is safe
    // because the header section cannot contain non-ASCII in well-formed
    // MIME, and we only need it for address extraction — the raw bytes are
    // sent to SMTP intact.
    let header_end = mime_content
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| {
            mime_content
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|p| p + 2)
        })
        .unwrap_or(mime_content.len());
    let header_section = String::from_utf8_lossy(&mime_content[..header_end]);

    let to_addr = RE_TO
        .captures(&header_section)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());
    let from_header = RE_FROM
        .captures(&header_section)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().trim().to_string());

    // Verify the From address matches the authenticated user to prevent spoofing
    let from_addr_str = match from_header {
        Some(f) => f,
        None => {
            tracing::warn!("SendMail: Missing From header");
            return SEND_MAIL_ERROR.to_string();
        }
    };
    let from_mailbox = match from_addr_str.parse::<lettre::message::Mailbox>() {
        Ok(mb) => mb,
        Err(e) => {
            tracing::warn!("SendMail: Malformed From header '{}': {}", from_addr_str, e);
            return SEND_MAIL_ERROR.to_string();
        }
    };
    let from_email = from_mailbox.email.to_string();
    // Compare the from address against the authenticated user. The auth username
    // may be a full email (user@domain) or just the local part (user). Accept
    // the message if the full email matches, or if the auth username has no '@'
    // and matches the local part of the from address AND the domain matches the
    // configured mail domain (to prevent domain spoofing).
    let from_matches = if authenticated_user.contains('@') {
        from_email.eq_ignore_ascii_case(authenticated_user)
    } else {
        if let Some((local, domain)) = from_email.rsplit_once('@') {
            local.eq_ignore_ascii_case(authenticated_user)
                && domain.eq_ignore_ascii_case(&config.mail_domain)
        } else {
            false
        }
    };
    if !from_matches {
        tracing::warn!(
            "SendMail: From address '{}' does not match authenticated user",
            from_email
        );
        return SEND_MAIL_ERROR.to_string();
    }
    let status = if let Some(to) = to_addr {
        // Parse potentially multiple To: addresses using RFC 5322 mailbox-list
        // parsing. Naive comma-splitting breaks quoted display names such as
        // "Doe, John" <john@example.com>.
        let to_mailboxes: Vec<lettre::message::Mailbox> =
            match to.parse::<lettre::message::Mailboxes>() {
                Ok(mbs) => mbs.into(),
                Err(e) => {
                    tracing::warn!("SendMail: invalid To address in '{}': {}", to, e);
                    Vec::new()
                }
            };

        if to_mailboxes.is_empty() {
            tracing::warn!("SendMail: no valid To addresses in '{}'", to);
            return SEND_MAIL_ERROR.to_string();
        }

        // Build an SMTP envelope from the validated addresses.  The full raw
        // MIME content is sent as-is so that multipart structure, Content-Type
        // headers, attachments, and any other MIME headers are preserved.
        let to_addresses: Vec<lettre::Address> =
            to_mailboxes.into_iter().map(|mb| mb.email).collect();
        let envelope = match Envelope::new(Some(from_mailbox.email), to_addresses) {
            Ok(env) => env,
            Err(e) => {
                tracing::error!("Failed to build SMTP envelope: {}", e);
                return SEND_MAIL_ERROR.to_string();
            }
        };

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

        let mut builder = {
            let b = match scheme.as_str() {
                "smtps" => {
                    // Implicit TLS (port 465): relay() configures TLS::Wrapper internally.
                    match AsyncSmtpTransport::<Tokio1Executor>::relay(smtp_host) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::error!("Failed to create SMTP relay transport: {}", e);
                            return SEND_MAIL_ERROR.to_string();
                        }
                    }
                }
                "smtp" => {
                    // Plain SMTP (port 25): use builder_dangerous for unencrypted connections.
                    AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
                }
                _ => {
                    // Default to STARTTLS (port 587).
                    match AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host) {
                        Ok(b) => b,
                        Err(e) => {
                            tracing::error!("Failed to create SMTP STARTTLS transport: {}", e);
                            return SEND_MAIL_ERROR.to_string();
                        }
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
        if !user.is_empty() {
            if let Some(pass) = smtp_url.password() {
                let pass = percent_decode_str(pass).decode_utf8_lossy();
                builder =
                    builder.credentials(lettre::transport::smtp::authentication::Credentials::new(
                        user.into_owned(),
                        pass.into_owned(),
                    ));
            }
        }

        let mailer = builder.build();

        // Send the full raw MIME content to preserve multipart structure,
        // Content-Type headers, attachments, and all other MIME headers.
        match mailer.send_raw(&envelope, &mime_content).await {
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
        r#"<SendMail xmlns="ComposeMail:"><Status>{}</Status></SendMail>"#,
        status
    )
}

async fn handle_meeting_response(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    xml: &str,
    authenticated_user: &str,
) -> String {
    let user_email = if authenticated_user.contains('@') {
        authenticated_user.to_string()
    } else {
        format!("{}@{}", authenticated_user, config.mail_domain)
    };

    let req: MeetingResponseRequest = match quick_xml::de::from_str(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("MeetingResponse: failed to parse request XML: {}", e);
            return error_xml(400, "Missing UID");
        }
    };
    let uid = &req.request.request_id;
    let response_code = req.request.user_response;
    if uid.is_empty() {
        return error_xml(400, "Missing UID");
    }

    let event_id = match jmap_client::find_event_by_uid(session, uid).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("Failed to find event by UID '{}': {}", uid, e);
            return error_xml(400, "EventNotFound");
        }
    };
    let status_str = match response_code {
        1 => "accepted",
        2 => "tentative",
        3 => "declined",
        _ => "needs-action",
    };
    if let Err(e) =
        jmap_client::update_participant_status(session, &event_id, &user_email, status_str).await
    {
        tracing::error!("MeetingResponse update failed: {}", e);
        return error_xml(500, "ParticipantUpdateFailed");
    }

    format!(
        r#"<MeetingResponse xmlns="AirSync:"><Result><Status>1</Status><CalendarId>{}</CalendarId></Result></MeetingResponse>"#,
        utils::escape_xml(&event_id)
    )
}

async fn handle_search(session: &jmap_client::JmapSession, xml: &str) -> String {
    #[derive(Debug, Deserialize)]
    struct SearchRequest {
        #[serde(rename = "Store")]
        store: SearchStore,
    }

    #[derive(Debug, Deserialize)]
    struct SearchStore {
        #[serde(rename = "Name", default)]
        name: Option<String>,
        #[serde(rename = "Query", default)]
        query: Option<SearchQuery>,
        #[serde(rename = "Options", default)]
        options: Option<SearchOptions>,
    }

    #[derive(Debug, Deserialize)]
    struct SearchQuery {
        #[serde(rename = "FreeText", default)]
        free_text: Option<String>,
    }

    #[derive(Debug, Deserialize)]
    struct SearchOptions {
        #[serde(rename = "Range", default)]
        range: Option<String>,
    }

    let req: SearchRequest = match quick_xml::de::from_str(xml) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Search XML Parse Error: {:?}", e);
            return error_xml(400, "BadRequest");
        }
    };

    let query = req
        .store
        .query
        .and_then(|q| q.free_text)
        .unwrap_or_default();
    let (mut range_start, mut range_max) = (0, 100);

    if let Some(range_str) = req.store.options.and_then(|o| o.range) {
        if let Some((start, end)) = range_str.split_once('-')
            && let Ok(m) = start.trim().parse::<usize>()
            && let Ok(n) = end.trim().parse::<usize>()
        {
            range_start = m;
            range_max = n.saturating_sub(m).saturating_add(1).min(1000);
        }
    }
    let results = match jmap_client::search_principals(session, &query).await {
        Ok(results) => results,
        Err(e) => {
            tracing::error!("Failed to search principals: {}", e);
            Vec::new()
        }
    };
    let results: Vec<_> = results
        .into_iter()
        .skip(range_start)
        .take(range_max)
        .collect();
    let mut results_xml = String::new();
    for p in results {
        results_xml.push_str(&format!(r#"<Result><Properties><DisplayName>{}</DisplayName><EmailAddress>{}</EmailAddress></Properties></Result>"#, utils::escape_xml(&p.name), utils::escape_xml(&p.email)));
    }
    format!(
        r#"<Search xmlns="Search:"><Status>1</Status><Response><Store><Status>1</Status>{}</Store></Response></Search>"#,
        results_xml
    )
}

async fn handle_item_operations(session: &jmap_client::JmapSession, req: ItemOpsReq) -> String {
    let mut fetches = String::new();
    for fetch in req.fetch.into_iter() {
        let id_opt = fetch.server_id.or(fetch.file_reference);
        if let Some(id) = id_opt {
            match jmap_client::get_event_by_id(session, &id).await {
                Ok(event) => {
                    fetches.push_str(&format!(r#"<Fetch><Status>1</Status><ServerId>{}</ServerId><Properties><AirSyncBase:Body><AirSyncBase:Type>1</AirSyncBase:Type><AirSyncBase:Data>{}</AirSyncBase:Data></AirSyncBase:Body></Properties></Fetch>"#, utils::escape_xml(&id), utils::escape_xml(event.description.as_deref().unwrap_or(""))));
                }
                Err(e) => {
                    tracing::warn!("Failed to get event by id '{}': {}", id, e);
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
    let has_client_commands = coll.commands.is_some();

    // Fetch the stored sync state (SyncKey + JMAP state) *before* processing
    // client commands.  We need the JMAP state for change-detection below.
    let stored_state = match db::get_sync_state_full(config, user, device_id, &collection_id).await
    {
        Ok(state) => state,
        Err(e) => {
            tracing::error!(
                user = user, device_id = device_id, collection = %collection_id,
                "Sync: failed to fetch sync state: {e} — returning server error so client retries without reset"
            );
            return format!(
                r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>5</Status></Collection></Collections></Sync>"#,
                utils::escape_xml(&old_sync_key),
                utils::escape_xml(&collection_id)
            );
        }
    };

    // Atomically validate and claim the incoming SyncKey when the client is
    // not performing an initial sync (SyncKey "0").  The claim uses a
    // conditional UPDATE (WHERE sync_key = <expected>) so that only one
    // concurrent request can succeed — the DB row is the single source of
    // truth and acts as a compare-and-swap guard.
    //
    // ActiveSync Status 3 = "Invalid or mismatched SyncKey" — the device
    // must re-issue a SyncKey 0 (full re-sync) to recover.
    if old_sync_key != "0" {
        if stored_state.is_some() {
            // Generate a temporary placeholder key for the claim.  The final
            // SyncKey issued to the client is computed later and written via
            // `update_sync_state`.
            let claim_key = Uuid::new_v4().to_string();
            let claim_result = db::claim_sync_key(
                config,
                user,
                device_id,
                &collection_id,
                &old_sync_key,
                &claim_key,
            )
            .await;
            match claim_result {
                Ok(true) => { /* claim succeeded, continue */ }
                Ok(false) => {
                    tracing::warn!(
                        "Sync: SyncKey claim failed for user={}, device={}, collection={}: \
                         client sent '{}' — another request already consumed it or the key \
                         does not match",
                        user,
                        device_id,
                        collection_id,
                        old_sync_key
                    );
                    // Status 3 tells the client its SyncKey is invalid.
                    return format!(
                        r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>3</Status></Collection></Collections></Sync>"#,
                        utils::escape_xml(&old_sync_key),
                        utils::escape_xml(&collection_id)
                    );
                }
                Err(e) => {
                    tracing::error!(
                        "Sync: transient DB error during SyncKey claim for user={}, device={}, \
                         collection={}: {} — returning server error so client retries without reset",
                        user,
                        device_id,
                        collection_id,
                        e
                    );
                    // The claim may have partially succeeded (the DB
                    // UPDATE could have gone through even though we got a
                    // transport/parse error on the response).  Restore the
                    // original SyncKey so the client can retry with the
                    // same key instead of being stuck with an invalid one.
                    if let Err(rollback_err) = db::claim_sync_key(
                        config,
                        user,
                        device_id,
                        &collection_id,
                        &claim_key,
                        &old_sync_key,
                    )
                    .await
                    {
                        tracing::error!(
                            "Sync: SyncKey rollback ALSO failed for user={}, device={}, \
                             collection={}: {} — the stored SyncKey may be stuck on an \
                             unreachable claim key; manual intervention or a client re-sync \
                             (SyncKey 0) may be required",
                            user,
                            device_id,
                            collection_id,
                            rollback_err
                        );
                    }
                    // Status 5 = server error — the client should retry
                    // without discarding its local state.
                    return format!(
                        r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>5</Status></Collection></Collections></Sync>"#,
                        utils::escape_xml(&old_sync_key),
                        utils::escape_xml(&collection_id)
                    );
                }
            }
        } else {
            tracing::warn!(
                "Sync: missing stored SyncKey for user={}, device={}, collection={} with client SyncKey '{}' — rejecting to prevent stale/replayed command replay",
                user,
                device_id,
                collection_id,
                old_sync_key
            );
            return format!(
                r#"<Sync xmlns="AirSync:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>3</Status></Collection></Collections></Sync>"#,
                utils::escape_xml(&old_sync_key),
                utils::escape_xml(&collection_id)
            );
        }
    }

    // Capture the JMAP state *before* processing client commands so that
    // change-detection (below) only returns true server-side changes and
    // does not echo the client's own modifications back to the device.
    let pre_command_jmap_state = match jmap_client::get_calendar_state(session).await {
        Ok(s) => s,
        Err(e) => {
            // Restore the old SyncKey so the client can retry without being
            // forced into a full resync.  No commands have been processed yet,
            // so replaying the request is safe.
            if old_sync_key != "0"
                && let Some(ref st) = stored_state
            {
                db::update_sync_state(
                    config,
                    user,
                    device_id,
                    &collection_id,
                    &old_sync_key,
                    &st.jmap_state,
                )
                .await;
            }
            return error_xml(500, "JMAPStateError");
        }
    };

    // On partial failure, fall through to the normal change-detection
    // path.  The SyncKey will advance and the post-command JMAP state
    // will reflect any successfully-applied operations.  The device
    // moves forward and will not replay succeeded commands.
    //
    // Failed commands are reported individually via <Responses> so
    // the device knows which ones to retry.
    let cmd_results = if let Some(cmds) = coll.commands {
        process_client_commands(session, cmds, &config.timezone).await
    } else {
        CommandResults::default()
    };
    let responses_xml = cmd_results.to_responses_xml();

    // After client commands have been applied, re-fetch the state so the
    // value stored in the DB reflects the post-command server state.  If no
    // client commands were sent, the state has not changed.
    let post_command_jmap_state = if has_client_commands {
        match jmap_client::get_calendar_state(session).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!("Failed to get post-command JMAP state: {}", e);
                if old_sync_key == "0" {
                    // Initial sync with client commands: commands have already
                    // been applied (e.g. calendar events created) but the
                    // post-command state fetch failed.  We must NOT fall
                    // through to the full event fetch because:
                    //   1. The full fetch would include events just created by
                    //      the client commands, sending them back as Add and
                    //      causing duplicate events on the device.
                    //   2. Since SyncKey "0" has no claim guard, the client
                    //      could retry with SyncKey "0" again, replaying its
                    //      commands in a loop.
                    //
                    // Persist an empty JMAP state sentinel with a new SyncKey
                    // so the client advances past SyncKey "0" (preventing
                    // command replay).  The empty sentinel causes the next
                    // sync to take the full-fetch path (see prev_state
                    // filtering below), ensuring pre-existing calendar events
                    // are delivered to the device.
                    let new_key = Uuid::new_v4().to_string();
                    tracing::warn!(
                        "Sync: post-command JMAP state fetch failed during initial sync \
                         (SyncKey \"0\") with client commands for user={}, device={}, \
                         collection={} — advancing SyncKey to prevent command replay; \
                         full event fetch deferred to next sync",
                        user,
                        device_id,
                        collection_id
                    );
                    db::update_sync_state(config, user, device_id, &collection_id, &new_key, "")
                        .await;
                    return format!(
                        r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status><Commands>{}</Commands>{}</Collection></Collections></Sync>"#,
                        utils::escape_xml(&new_sync_key),
                        utils::escape_xml(&collection_id),
                        items_xml,
                        responses_xml
                    );
                } else {
                    // Non-initial sync: the post-command state fetch failed,
                    // but client commands may have already been applied.
                    // Persist the pre-command JMAP state with a new SyncKey so
                    // the client advances (avoiding command replay) and the
                    // next sync picks up changes via normal change-detection.
                    let new_key = Uuid::new_v4().to_string();
                    db::update_sync_state(
                        config,
                        user,
                        device_id,
                        &collection_id,
                        &new_key,
                        &pre_command_jmap_state,
                    )
                    .await;
                    return format!(
                        r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands></Commands></Collection></Collections></Sync>"#,
                        utils::escape_xml(&new_key),
                        utils::escape_xml(&collection_id),
                        responses_xml
                    );
                }
            }
        }
    } else {
        pre_command_jmap_state.clone()
    };

    // Map stored JMAP state to `None` when it is the empty sentinel written
    // after a failed post-command state fetch during initial sync.  This
    // ensures the next sync takes the full-fetch path instead of the
    // change-detection path, so pre-existing calendar events are delivered.
    let stored_jmap_state = stored_state
        .as_ref()
        .map(|s| s.jmap_state.clone())
        .unwrap_or_default();
    let prev_state = stored_state.map(|s| s.jmap_state).filter(|s| !s.is_empty());

    // Detect changes since the last sync.  When client commands were
    // processed, the changes may include the client's own writes — this is
    // preferable to silently dropping concurrent server-side changes.
    let (items_xml, new_sync_key, jmap_state_to_persist) = match prev_state {
        Some(prev_jmap_state) if old_sync_key != "0" => {
            if prev_jmap_state == pre_command_jmap_state
                && pre_command_jmap_state == post_command_jmap_state
            {
                // No server-side changes and no client-originated changes —
                // nothing to send.  Still advance the SyncKey so the client
                // sees a successful round-trip (ActiveSync requires the key
                // to change on every response).
                let new_key = Uuid::new_v4().to_string();
                db::update_sync_state(
                    config,
                    user,
                    device_id,
                    &collection_id,
                    &new_key,
                    &prev_jmap_state,
                )
                .await;
                return format!(
                    r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands></Commands></Collection></Collections></Sync>"#,
                    utils::escape_xml(&new_key),
                    utils::escape_xml(&collection_id),
                    responses_xml
                );
            }
            // Fetch changes since the last persisted state.  When client
            // commands were processed, this may include the client's own
            // writes echoed back (harmless — the device will merge/ignore
            // duplicates), but it also ensures any concurrent server-side
            // changes are not silently dropped.
            let err_ctx = SyncErrorContext {
                config,
                user,
                device_id,
                collection_id: &collection_id,
                old_sync_key: &old_sync_key,
                prev_jmap_state: &prev_jmap_state,
                has_client_commands,
                responses_xml: &responses_xml,
            };
            let changes = match jmap_client::get_calendar_changes(session, &prev_jmap_state).await {
                Ok(c) => c,
                Err(e) => {
                    return handle_sync_change_error("get_calendar_changes", &e, &err_ctx).await;
                }
            };
            match render_changes(session, changes, &config.timezone).await {
                Ok(result) => result,
                Err(e) => {
                    return handle_sync_change_error("render_changes", &e, &err_ctx).await;
                }
            }
        }
        _ => {
            // Either prev_state is None, or old_sync_key is "0" (client reset)
            let events = match jmap_client::get_calendar_events(session).await {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("Failed to fetch calendar events: {}", e);
                    // Client commands were already applied — advance the
                    // SyncKey to prevent command replay on retry.  This must
                    // happen regardless of whether old_sync_key is "0"
                    // (initial sync) or not; otherwise the client will resend
                    // the same commands on the next attempt.
                    if has_client_commands {
                        let new_key = Uuid::new_v4().to_string();
                        db::update_sync_state(
                            config,
                            user,
                            device_id,
                            &collection_id,
                            &new_key,
                            if old_sync_key == "0" {
                                ""
                            } else {
                                &stored_jmap_state
                            },
                        )
                        .await;
                        return format!(
                            r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands></Commands></Collection></Collections></Sync>"#,
                            utils::escape_xml(&new_key),
                            utils::escape_xml(&collection_id),
                            responses_xml
                        );
                    }
                    // When old_sync_key != "0", claim_sync_key already replaced
                    // the DB's sync_key with a temporary placeholder.  Restore
                    // the original SyncKey so the client can retry with the
                    // same key instead of hitting Status 3 (invalid key).
                    if old_sync_key != "0" {
                        db::update_sync_state(
                            config,
                            user,
                            device_id,
                            &collection_id,
                            &old_sync_key,
                            &stored_jmap_state,
                        )
                        .await;
                    }
                    return error_xml(500, "CalendarEventsError");
                }
            };
            let mut xml = String::new();
            let new_key = Uuid::new_v4().to_string();
            for event in events {
                xml.push_str(&render_event_xml(event, "Add", &config.timezone));
            }
            (xml, new_key, post_command_jmap_state)
        }
    };

    db::update_sync_state(
        config,
        user,
        device_id,
        &collection_id,
        &new_sync_key,
        &jmap_state_to_persist,
    )
    .await;

    format!(
        r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands>{}</Commands></Collection></Collections></Sync>"#,
        utils::escape_xml(&new_sync_key),
        utils::escape_xml(&collection_id),
        responses_xml,
        items_xml
    )
}

fn render_event_xml(event: jmap_client::JmapEvent, mode: &str, tz_str: &str) -> String {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);

    // Avoid silently defaulting to Unix epoch on parse errors.
    // If parsing fails, log and fall back to using the original JMAP string as-is.
    let start_str = if let Ok(dt) = DateTime::parse_from_rfc3339(&event.start) {
        dt.with_timezone(&tz)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    } else if let Ok(naive) = NaiveDateTime::parse_from_str(&event.start, "%Y-%m-%dT%H:%M:%S") {
        naive.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        tracing::warn!(
            "Invalid JMAP event start timestamp for event id {:?}: '{}'",
            event.id,
            event.start,
        );
        event.start.clone()
    };

    let end_str = if let Ok(dt) = DateTime::parse_from_rfc3339(&event.end) {
        dt.with_timezone(&tz)
            .format("%Y-%m-%dT%H:%M:%S")
            .to_string()
    } else if let Ok(naive) = NaiveDateTime::parse_from_str(&event.end, "%Y-%m-%dT%H:%M:%S") {
        naive.format("%Y-%m-%dT%H:%M:%S").to_string()
    } else {
        tracing::warn!(
            "Invalid JMAP event end timestamp for event id {:?}: '{}'",
            event.id,
            event.end,
        );
        event.end.clone()
    };

    let mut attendees_xml = String::new();
    if let Some(attendees) = &event.participants {
        attendees_xml.push_str("<Calendar:Attendees>");
        for att in attendees {
            attendees_xml.push_str(&format!(r#"<Calendar:Attendee><Calendar:Email>{}</Calendar:Email><Calendar:Name>{}</Calendar:Name></Calendar:Attendee>"#, utils::escape_xml(&att.email), utils::escape_xml(&att.name)));
        }
        attendees_xml.push_str("</Calendar:Attendees>");
    }
    let recurrence_xml = if let Some(rules) = &event.recurrence_rules {
        // EAS only supports a single recurrence; use the first rule
        rules
            .first()
            .map(recurrence_rule_to_eas)
            .unwrap_or_default()
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
        if event.show_without_time { "1" } else { "0" },
        recurrence_xml,
        attendees_xml,
        body_content,
        mode
    )
}

fn recurrence_rule_to_eas(rule: &jmap_client::RecurrenceRule) -> String {
    let has_byday = rule.by_day.is_some();

    // Compute EAS day-of-week bitmask and optional week-of-month from NDay objects.
    // EAS only supports a single WeekOfMonth value, so we keep the first
    // nth_of_period encountered and warn if subsequent entries disagree.
    let mut day_of_week: i32 = 0;
    let mut week_of_month: Option<i32> = None;
    if let Some(days) = &rule.by_day {
        for nday in days {
            let mask = match nday.day.as_str() {
                "mo" => 2,
                "tu" => 4,
                "we" => 8,
                "th" => 16,
                "fr" => 32,
                "sa" => 64,
                "su" => 1,
                _ => 0,
            };
            day_of_week |= mask;
            if let Some(nth) = nday.nth_of_period {
                let eas_wom = if nth == -1 { 5 } else { nth };
                match week_of_month {
                    None => week_of_month = Some(eas_wom),
                    Some(existing) if existing != eas_wom => {
                        tracing::warn!(
                            "Recurrence rule has conflicting nthOfPeriod values \
                             ({} vs {}); EAS only supports one WeekOfMonth — \
                             keeping the first value ({})",
                            existing,
                            eas_wom,
                            existing
                        );
                    }
                    _ => {} // same value, no conflict
                }
            }
        }
    }

    // bySetPosition can also express week-of-month for non-relative rules
    if week_of_month.is_none()
        && let Some(positions) = &rule.by_set_position
        && let Some(&pos) = positions.first()
    {
        week_of_month = Some(if pos == -1 { 5 } else { pos });
    }

    let day_of_month: Option<i32> = rule.by_month_day.as_ref().and_then(|v| v.first().copied());

    let month_of_year: Option<i32> = rule
        .by_month
        .as_ref()
        .and_then(|v| v.first())
        .and_then(|s| s.parse::<i32>().ok());

    // Determine EAS recurrence type:
    //   0 = Daily, 1 = Weekly,
    //   2 = Monthly (absolute, by day-of-month),
    //   3 = Monthly (relative, by day-of-week + week-of-month),
    //   5 = Yearly (absolute), 6 = Yearly (relative)
    let eas_type = match rule.frequency.as_str() {
        "daily" => "0",
        "weekly" => "1",
        "monthly" => {
            if has_byday {
                "3"
            } else {
                "2"
            }
        }
        "yearly" => {
            if has_byday {
                "6"
            } else {
                "5"
            }
        }
        _ => "0",
    };

    let interval = rule.interval.unwrap_or(1);

    // Build optional child elements in the strict order required by MS-ASCAL:
    // Type, Occurrences, Interval, WeekOfMonth, DayOfWeek, MonthOfYear, Until, DayOfMonth, ...
    let mut extra_xml = String::new();
    if let Some(wom) = week_of_month {
        extra_xml.push_str(&format!(
            "<Calendar:WeekOfMonth>{}</Calendar:WeekOfMonth>",
            wom
        ));
    }
    if day_of_week != 0 {
        extra_xml.push_str(&format!(
            "<Calendar:DayOfWeek>{}</Calendar:DayOfWeek>",
            day_of_week
        ));
    }
    if let Some(moy) = month_of_year {
        extra_xml.push_str(&format!(
            "<Calendar:MonthOfYear>{}</Calendar:MonthOfYear>",
            moy
        ));
    }
    if let Some(dom) = day_of_month {
        extra_xml.push_str(&format!(
            "<Calendar:DayOfMonth>{}</Calendar:DayOfMonth>",
            dom
        ));
    }

    format!(
        r#"<Calendar:Recurrence><Calendar:Type>{}</Calendar:Type><Calendar:Interval>{}</Calendar:Interval>{}</Calendar:Recurrence>"#,
        eas_type, interval, extra_xml
    )
}

async fn process_client_commands(
    session: &jmap_client::JmapSession,
    cmds: Commands,
    tz_str: &str,
) -> CommandResults {
    let tz: Tz = tz_str.parse().unwrap_or(chrono_tz::UTC);
    let mut failures: Vec<CommandFailure> = Vec::new();
    let mut add_successes: Vec<AddSuccess> = Vec::new();
    if let Some(add_cmds) = cmds.add {
        let cal_id = match jmap_client::get_default_calendar_id(session).await {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!(
                    "ActiveSync Add failed: unable to determine default calendar id: {}",
                    e
                );
                None
            }
        };
        // Collect valid events for a single batched JMAP CalendarEvent/set
        // call instead of issuing one network round-trip per Add command.
        let mut batch: Vec<(String, jmap_client::JmapEvent)> = Vec::new();
        let mut batch_client_ids: Vec<String> = Vec::new();
        for add_cmd in add_cmds {
            let client_id = add_cmd.client_id;
            let Some(ref _cal_id) = cal_id else {
                // Calendar ID lookup failed — mark every Add as failed so
                // the device retries them on the next Sync.
                failures.push(CommandFailure::Add { client_id });
                continue;
            };
            let data = add_cmd.application_data;
            let (Some(start_raw), Some(end_raw)) = (data.start.as_deref(), data.end.as_deref())
            else {
                tracing::warn!(
                    "ActiveSync Add rejected: missing start/end time (client_id={:?})",
                    client_id
                );
                failures.push(CommandFailure::Add { client_id });
                continue;
            };
            let start_utc = utils::parse_local_to_utc(start_raw, tz);
            let end_utc = utils::parse_local_to_utc(end_raw, tz);
            let attendees: Vec<jmap_client::Participant> = data
                .attendees
                .map(|a| {
                    a.items
                        .into_iter()
                        .map(|att| jmap_client::Participant {
                            email: att.email,
                            name: att.name,
                            status: None,
                            participant_id: None,
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
                show_without_time: data.all_day_event.unwrap_or(0) == 1,
                recurrence_rules: data.recurrence.map(|r| vec![build_recurrence_rule(r)]),
                updated: None,
            };
            batch_client_ids.push(client_id.clone());
            batch.push((client_id, event));
        }
        if let Some(ref cal_id) = cal_id
            && !batch.is_empty()
        {
            match jmap_client::push_events(session, batch, cal_id).await {
                Ok(result) => {
                    if let Some(ref e) = result.chunk_error {
                        tracing::error!(
                            "ActiveSync batch Add partially failed: {}; \
                             {} created, {} not created",
                            e,
                            result.created.len(),
                            result.not_created.len()
                        );
                    }
                    for (client_id, created) in result.created {
                        add_successes.push(AddSuccess {
                            client_id,
                            server_id: created.id,
                        });
                    }
                    for (client_id, desc) in result.not_created {
                        tracing::error!(
                            "ActiveSync Add failed for client_id {}: {}",
                            client_id,
                            desc
                        );
                        failures.push(CommandFailure::Add { client_id });
                    }
                }
                Err(e) => {
                    tracing::error!("ActiveSync batch Add failed: {}", e);
                    for client_id in batch_client_ids {
                        failures.push(CommandFailure::Add { client_id });
                    }
                }
            }
        }
    }
    let mut change_patches: Vec<(String, serde_json::Map<String, serde_json::Value>)> = Vec::new();
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
                serde_json::json!(utils::parse_local_to_utc(&s, tz)),
            );
        }
        if let Some(e) = data.end {
            patch.insert(
                "end".into(),
                serde_json::json!(utils::parse_local_to_utc(&e, tz)),
            );
        }
        if let Some(b) = data.body {
            patch.insert("description".into(), serde_json::json!(b.data));
        }
        if let Some(r) = data.recurrence {
            patch.insert(
                "recurrenceRules".into(),
                serde_json::json!(vec![build_recurrence_rule(r)]),
            );
        }
        if !patch.is_empty() {
            change_patches.push((id, patch));
        }
    }
    if !change_patches.is_empty() {
        let change_ids: Vec<String> = change_patches.iter().map(|(id, _)| id.clone()).collect();
        match jmap_client::batch_patch_events(session, change_patches).await {
            Ok(result) => {
                if let Some(ref e) = result.chunk_error {
                    tracing::error!(
                        "ActiveSync batch Change partially failed: {}; \
                         {} updated, {} not updated",
                        e,
                        result.updated.len(),
                        result.not_updated.len()
                    );
                }
                for (server_id, desc) in result.not_updated {
                    tracing::error!("ActiveSync Update failed for id {}: {}", server_id, desc);
                    failures.push(CommandFailure::Change { server_id });
                }
            }
            Err(e) => {
                tracing::error!("ActiveSync batch Change failed: {}", e);
                for server_id in change_ids {
                    failures.push(CommandFailure::Change { server_id });
                }
            }
        }
    }
    if let Some(deletes) = cmds.delete
        && !deletes.is_empty()
    {
        let ids: Vec<String> = deletes.into_iter().map(|d| d.server_id).collect();
        match jmap_client::destroy_events(session, ids.clone()).await {
            Ok(destroy_result) => {
                for (sid, err_type) in destroy_result.not_destroyed {
                    tracing::error!("ActiveSync Delete failed for id {}: {}", sid, err_type);
                    failures.push(CommandFailure::Delete { server_id: sid });
                }
                if let Some(e) = destroy_result.chunk_error {
                    tracing::error!("ActiveSync Delete chunk error: {}", e);
                }
            }
            Err(e) => {
                tracing::error!("ActiveSync Delete failed: {}", e);
                for sid in ids {
                    failures.push(CommandFailure::Delete { server_id: sid });
                }
            }
        }
    }
    if !failures.is_empty() {
        tracing::error!(
            "ActiveSync command failures: {} command(s) failed",
            failures.len()
        );
    }
    CommandResults {
        failures,
        add_successes,
    }
}

fn build_recurrence_rule(r: Recurrence) -> jmap_client::RecurrenceRule {
    let frequency = match r.r#type {
        0 => "daily",
        1 => "weekly",
        2 | 3 => "monthly",
        5 | 6 => "yearly",
        unknown => {
            tracing::warn!(
                "Unknown EAS recurrence type '{}', defaulting to 'daily'",
                unknown
            );
            "daily"
        }
    }
    .to_string();

    let interval = if r.interval > 1 {
        Some(r.interval as u32)
    } else {
        None
    };

    // EAS relative types (3 = monthly relative, 6 = yearly relative) use
    // WeekOfMonth to express nth-of-period on the NDay objects.
    let is_relative = matches!(r.r#type, 3 | 6);

    // EAS encodes "last day of month" as WeekOfMonth=5 + DayOfWeek=127
    // (all seven day-bits set). In JSCalendar this is simply byMonthDay=[-1];
    // expanding it into seven NDay{-1,day} entries would produce up to seven
    // occurrences per month instead of one.
    let all_days_mask: i32 = 127; // su|mo|tu|we|th|fr|sa
    let is_last_day_of_period =
        is_relative && r.week_of_month == Some(5) && r.day_of_week == Some(all_days_mask);

    let by_day = if is_last_day_of_period {
        // Handled via byMonthDay=[-1] below.
        None
    } else {
        r.day_of_week.and_then(|dow| {
            let day_bits: &[(&str, i32)] = &[
                ("su", 1),
                ("mo", 2),
                ("tu", 4),
                ("we", 8),
                ("th", 16),
                ("fr", 32),
                ("sa", 64),
            ];
            let matched: Vec<(&str, i32)> = day_bits
                .iter()
                .filter(|(_, mask)| (dow & mask) != 0)
                .copied()
                .collect();
            // For relative types (monthly/yearly relative), nthOfPeriod on
            // NDay means "the Nth <day> of the period" — e.g.
            // NDay{day:"mo", nthOfPeriod:2} means "the 2nd Monday".
            // When only one day bit is set this is correct.  When multiple
            // day bits are set, attaching nthOfPeriod to every NDay would
            // create one occurrence per day (e.g. "2nd Monday AND 2nd
            // Wednesday") instead of the EAS intent of "days in the Nth
            // week".  In the multi-day case, omit nthOfPeriod from the
            // NDay entries and use bySetPosition instead to constrain the
            // week (handled below).
            let multiple_days = matched.len() > 1;
            let days: Vec<jmap_client::NDay> = matched
                .iter()
                .map(|(name, _)| {
                    let nth = if is_relative && !multiple_days {
                        r.week_of_month.map(|wom| if wom == 5 { -1 } else { wom })
                    } else {
                        None
                    };
                    jmap_client::NDay {
                        r#type: "NDay".to_string(),
                        day: name.to_string(),
                        nth_of_period: nth,
                    }
                })
                .collect();
            if days.is_empty() { None } else { Some(days) }
        })
    };

    let by_month_day = if is_last_day_of_period {
        Some(vec![-1])
    } else {
        r.day_of_month.map(|dom| vec![dom])
    };

    // Count how many day bits are set for multi-day detection.
    let day_count = r.day_of_week.map_or(0, |dow| {
        [1, 2, 4, 8, 16, 32, 64]
            .iter()
            .filter(|&&mask| (dow & mask) != 0)
            .count()
    });

    // Use bySetPosition to express the week-of-month constraint when:
    //   - the rule is non-relative (original behaviour), OR
    //   - the rule is relative but has multiple day bits (see comment above
    //     in by_day construction).
    // For relative rules with a single day bit, nthOfPeriod on the NDay is
    // used instead (more precise).
    let by_set_position = if !is_relative || day_count > 1 {
        r.week_of_month
            .map(|wom| vec![if wom == 5 { -1 } else { wom }])
    } else {
        None
    };

    // RFC 8984 byMonth uses month number strings (e.g. "1", "2", …, "12")
    let by_month = r.month_of_year.map(|moy| vec![moy.to_string()]);

    jmap_client::RecurrenceRule {
        r#type: "RecurrenceRule".to_string(),
        frequency,
        interval,
        by_day,
        by_month_day,
        by_month,
        by_set_position,
        count: None,
        until: None,
    }
}

async fn render_changes(
    session: &jmap_client::JmapSession,
    changes: jmap_client::JmapChanges,
    tz_str: &str,
) -> Result<(String, String, String), jmap_client::JmapError> {
    let mut xml = String::new();
    let new_key = Uuid::new_v4().to_string();
    // Capture the authoritative JMAP state from the /changes response
    // *before* consuming the struct fields, so we persist the correct
    // state rather than a potentially stale snapshot.
    let new_state = changes.new_state.clone();
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
    Ok((xml, new_key, new_state))
}

/// Handle a JMAP error that occurred during the sync change-detection phase
/// (either `get_calendar_changes` or `render_changes`).
///
/// * **Transient** errors preserve the existing sync state.  If the client
///   already sent commands that were applied successfully, we return a success
///   response with a fresh SyncKey so the device does NOT replay them.
/// * **Non-transient** errors restore the original SyncKey so the client
///   can retry with the same key without being forced into a full re-sync.
async fn handle_sync_change_error(
    label: &str,
    error: &jmap_client::JmapError,
    ctx: &SyncErrorContext<'_>,
) -> String {
    if error.is_transient() {
        tracing::warn!("{label} failed (transient), preserving sync state: {error}");
        if ctx.has_client_commands {
            let new_key = Uuid::new_v4().to_string();
            db::update_sync_state(
                ctx.config,
                ctx.user,
                ctx.device_id,
                ctx.collection_id,
                &new_key,
                ctx.prev_jmap_state,
            )
            .await;
            return format!(
                r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands></Commands></Collection></Collections></Sync>"#,
                utils::escape_xml(&new_key),
                utils::escape_xml(ctx.collection_id),
                ctx.responses_xml
            );
        }
        // No client commands — restore the original SyncKey so the client
        // can retry with the same key without being forced into a full
        // re-sync.  The claim_sync_key earlier replaced the DB's sync_key
        // with a temporary placeholder; reverting it here makes the
        // transient failure invisible to the client.
        db::update_sync_state(
            ctx.config,
            ctx.user,
            ctx.device_id,
            ctx.collection_id,
            ctx.old_sync_key,
            ctx.prev_jmap_state,
        )
        .await;
        return error_xml(500, "CalendarChangesError");
    } else if ctx.has_client_commands {
        // Non-transient error, but client commands were already applied.
        // Preserve the sync state with a fresh SyncKey so the client
        // advances and does NOT replay the commands (which would create
        // duplicates).  Use the *previous* JMAP state (not the post-command
        // state) so the next sync's change-detection re-fetches all changes
        // since the last successful sync — including any server-side changes
        // that were never delivered to the device because change-detection
        // failed this time.  The client's own writes may be echoed back,
        // but that is harmless (the device merges/ignores duplicates) and
        // preferable to silently skipping server changes.
        tracing::error!(
            "{label} failed (non-transient) after client commands, \
             preserving sync state to prevent command replay: {error}"
        );
        let new_key = Uuid::new_v4().to_string();
        db::update_sync_state(
            ctx.config,
            ctx.user,
            ctx.device_id,
            ctx.collection_id,
            &new_key,
            ctx.prev_jmap_state,
        )
        .await;
        return format!(
            r#"<Sync xmlns="AirSync:" xmlns:Calendar="Calendar:" xmlns:AirSyncBase="AirSyncBase:"><Collections><Collection><SyncKey>{}</SyncKey><CollectionId>{}</CollectionId><Status>1</Status>{}<Commands></Commands></Collection></Collections></Sync>"#,
            utils::escape_xml(&new_key),
            utils::escape_xml(ctx.collection_id),
            ctx.responses_xml
        );
    } else {
        // Non-transient error, no client commands — the JMAP state is
        // permanently invalid (e.g. expired sinceState, parse failure).
        // Restoring the old SyncKey would trap the client in an infinite
        // retry loop hitting the same error.  Instead, delete the sync
        // state so the next request is treated as an initial sync
        // (SyncKey "0"), which lets the client recover.
        tracing::error!(
            "{label} failed (non-transient), deleting sync state to force re-sync: {error}"
        );
        if let Err(e) =
            db::delete_sync_state(ctx.config, ctx.user, ctx.device_id, ctx.collection_id).await
        {
            tracing::error!("{label} additionally failed to delete sync state: {e}");
        }
    }
    error_xml(500, "CalendarChangesError")
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
    #[serde(rename = "Commands", default)]
    commands: Option<Commands>,
}
#[derive(Debug, Deserialize)]
struct Commands {
    #[serde(rename = "Add", default)]
    add: Option<Vec<AddCommand>>,
    #[serde(rename = "Change", default)]
    change: Option<Vec<ChangeCommand>>,
    #[serde(rename = "Delete", default)]
    delete: Option<Vec<DeleteCommand>>,
}
#[derive(Debug, Deserialize)]
struct AddCommand {
    #[serde(rename = "ClientId")]
    client_id: String,
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
    #[serde(rename = "Fetch", default)]
    fetch: Vec<ItemOpsFetch>,
}
#[derive(Debug, Deserialize)]
struct ItemOpsFetch {
    #[serde(rename = "Store", default)]
    _store: Option<String>,
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
/// MeetingResponse request — the spec nests RequestId and UserResponse inside
/// a `<Request>` element.
#[derive(Debug, Deserialize)]
struct MeetingResponseRequest {
    #[serde(rename = "Request")]
    request: MeetingResponseEntry,
}
#[derive(Debug, Deserialize)]
struct MeetingResponseEntry {
    #[serde(rename = "RequestId")]
    request_id: String,
    #[serde(rename = "UserResponse")]
    user_response: i32,
}

/// FolderSync request — we only need the SyncKey from the client.
#[derive(Debug, Deserialize)]
struct FolderSyncRequest {
    #[serde(rename = "SyncKey")]
    sync_key: String,
}

async fn handle_folder_sync(
    session: &jmap_client::JmapSession,
    config: &AppConfig,
    user: &str,
    device_id: &str,
    xml: &str,
) -> String {
    db::register_device(config, user, device_id).await;

    let incoming_key = match quick_xml::de::from_str::<FolderSyncRequest>(xml) {
        Ok(r) => r.sync_key,
        Err(e) => {
            tracing::error!("FolderSync XML Parse Error: {:?}", e);
            return error_xml(400, "BadRequest");
        }
    };

    // The folder hierarchy is static (single calendar), so we use a fixed
    // non-zero SyncKey.  When the client already holds this key we return an
    // empty change set so it skips a redundant full folder re-sync.
    const FOLDER_SYNC_KEY: &str = "1";

    if incoming_key == FOLDER_SYNC_KEY {
        // Client is up-to-date — no folder changes.
        return format!(
            r#"<FolderSync xmlns="AirSync:"><Status>1</Status><SyncKey>{}</SyncKey><Changes><Count>0</Count></Changes></FolderSync>"#,
            FOLDER_SYNC_KEY
        );
    }

    // Initial sync (SyncKey "0" or unknown) — return the full folder list.
    let cal_id = match jmap_client::get_default_calendar_id(session).await {
        Ok(id) => id,
        Err(e) => {
            tracing::error!("FolderSync failed: unable to determine default calendar id: {e}");
            return error_xml(500, "NoCalendars");
        }
    };

    format!(
        r#"<FolderSync xmlns="AirSync:"><Status>1</Status><SyncKey>{}</SyncKey><Changes><Count>1</Count><Add><ServerId>{}</ServerId><ParentId>0</ParentId><DisplayName>Calendar</DisplayName><Type>8</Type></Add></Changes></FolderSync>"#,
        FOLDER_SYNC_KEY,
        utils::escape_xml(&cal_id)
    )
}
