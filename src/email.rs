// src/email.rs
//
// Email protocol operations for Exchange Gateway.
//
// Implements EWS and EAS email operations that translate between
// Microsoft Exchange protocols and Stalwart Mailserver via:
// - SMTP (port 465) for sending email
// - JMAP for reading/syncing email
// - CalDAV for calendar (unchanged)
//
// EWS Operations:
// - SendItem (MS-OXWSCORE §3.1.4.8)
// - CreateItem with MessageDisposition for email (MS-OXWSCORE §3.1.4.2)
// - GetItem for MessageType (MS-OXWSCORE §3.1.4.4)
// - FindItem for email folders
// - SyncFolderItems for email folders
//
// EAS Operations:
// - SendMail (MS-ASCMD §2.2.1.17)
// - SmartReply (MS-ASCMD §2.2.1.20)
// - SmartForward (MS-ASCMD §2.2.1.19)
// - Email Sync class (MS-ASEMAIL)

use crate::jmap::JmapEmail;
use crate::models::AppState;
use crate::util::xml_escape;
use secrecy::SecretString;
use std::sync::Arc;
use tracing::info;

/// Deterministically extract the email body content and its type from a JMAP email.
///
/// Per RFC 8621 §4.1.4, `bodyValues` is keyed by `partId` from `textBody`/`htmlBody`.
/// Using `HashMap::values().next()` is non-deterministic (Rust randomizes HashMap
/// iteration order) — it could return HTML instead of plain text, an empty part,
/// or even an attachment body value.
///
/// This function resolves the body deterministically:
/// 1. Look up `textBody[0].partId` in `bodyValues` → plain text body
/// 2. If no text body, look up `htmlBody[0].partId` in `bodyValues` → HTML body
/// 3. Fall back to empty string with Text type
///
/// Returns `(body_content: &str, is_html: bool)`.
fn extract_jmap_body(email: &JmapEmail) -> (&str, bool) {
    let bv = match email.body_values.as_ref() {
        Some(bv) => bv,
        None => return ("", false),
    };

    // Try plain text body first — per RFC 8621, textBody[].partId maps into bodyValues
    if let Some(text_parts) = email.text_body.as_ref()
        && let Some(first_part) = text_parts.first()
        && let Some(bv_entry) = bv.get(&first_part.part_id)
    {
        return (bv_entry.value.as_str(), false);
    }

    // Fall back to HTML body
    if let Some(html_parts) = email.html_body.as_ref()
        && let Some(first_part) = html_parts.first()
        && let Some(bv_entry) = bv.get(&first_part.part_id)
    {
        return (bv_entry.value.as_str(), true);
    }

    ("", false)
}

/// EWS Message Disposition types per MS-OXWSCORE §3.1.4.2
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MessageDisposition {
    SaveOnly,
    SendOnly,
    SendAndSaveCopy,
}

impl MessageDisposition {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "SaveOnly" => Some(Self::SaveOnly),
            "SendOnly" => Some(Self::SendOnly),
            "SendAndSaveCopy" => Some(Self::SendAndSaveCopy),
            _ => None,
        }
    }
}

/// Parsed EWS MessageType from CreateItem/SendItem request.
///
/// Per MS-OXWSCORE §2.2.4.25, MessageType extends ItemType with
/// email-specific fields like ToRecipients, From, etc.
#[derive(Clone, Debug, Default)]
pub struct EwsMessage {
    pub subject: String,
    pub body: String,
    pub body_type: String, // "Text" or "HTML"
    pub from: String,
    pub to_recipients: Vec<String>,
    pub cc_recipients: Vec<String>,
    pub bcc_recipients: Vec<String>,
    pub reply_to: Vec<String>,
    pub importance: String,
    pub item_id: Option<String>,
    pub change_key: Option<String>,
    /// For SmartReply/SmartForward — the original message reference
    pub references: Option<String>,
    pub in_reply_to: Option<String>,
}

/// Parse a MessageType from EWS SOAP XML body.
///
/// Extracts key email fields from the `<t:Message>` element within
/// a CreateItem or SendItem request.
pub fn parse_ews_message(body: &str) -> Option<EwsMessage> {
    // Subject is optional — emails without a subject are valid per RFC 5322 §3.6.5.
    // If missing, default to empty string. Previously, a missing <t:Subject> caused
    // this function to return None, making CreateItem fall through to calendar item
    // handling (silent misdispatch) and SendItem return ErrorItemNotFound (false
    // error for a valid no-subject email).
    let subject = extract_ews_tag_text(body, b"Subject").unwrap_or_default();

    // Extract body — EWS Body element has attributes: <t:Body BodyType="Text">...</t:Body>
    let mut body_content = String::new();
    let mut body_type = "Text".to_string();
    if let Some(body_start) = body.find("<t:Body") {
        let gt_pos = body[body_start..].find('>').unwrap_or(0);
        let content_start = body_start + gt_pos + 1;
        if let Some(end) = body[content_start..].find("</t:Body>") {
            body_content = body[content_start..content_start + end].to_string();
            let opening_tag = &body[body_start..body_start + gt_pos + 1];
            if let Some(bt_start) = opening_tag.find("BodyType=\"") {
                let bt_val_start = bt_start + "BodyType=\"".len();
                if let Some(bt_end) = opening_tag[bt_val_start..].find('"') {
                    body_type = opening_tag[bt_val_start..bt_val_start + bt_end].to_string();
                }
            }
        }
    }

    let from = extract_ews_email_address(body, b"From").unwrap_or_default();
    let to_recipients = extract_ews_email_addresses(body, b"ToRecipients");
    let cc_recipients = extract_ews_email_addresses(body, b"CcRecipients");
    let bcc_recipients = extract_ews_email_addresses(body, b"BccRecipients");
    let importance = extract_ews_tag_text(body, b"Importance").unwrap_or_else(|| "Normal".to_string());

    Some(EwsMessage {
        subject,
        body: body_content,
        body_type,
        from,
        to_recipients,
        cc_recipients,
        bcc_recipients,
        importance,
        ..Default::default()
    })
}

/// Extract text content from an EWS XML tag.
fn extract_ews_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let open = format!("<t:{}>", std::str::from_utf8(tag).ok()?);
    let close = format!("</t:{}>", std::str::from_utf8(tag).ok()?);

    if let Some(start) = xml.find(&open) {
        let content_start = start + open.len();
        if let Some(end) = xml[content_start..].find(&close) {
            return Some(xml[content_start..content_start + end].to_string());
        }
    }
    None
}

/// Extract a single email address from an EWS Mailbox element.
fn extract_ews_email_address(xml: &str, container: &[u8]) -> Option<String> {
    let container_str = std::str::from_utf8(container).ok()?;
    let open = format!("<t:{}>", container_str);
    if let Some(start) = xml.find(&open) {
        let rest = &xml[start..];
        // Find EmailAddress within this container
        if let Some(email_start) = rest.find("<t:EmailAddress>") {
            let email_content_start = email_start + "<t:EmailAddress>".len();
            if let Some(email_end) = rest[email_content_start..].find("</t:EmailAddress>") {
                return Some(rest[email_content_start..email_content_start + email_end].to_string());
            }
        }
    }
    None
}

/// Extract multiple email addresses from an EWS container element.
fn extract_ews_email_addresses(xml: &str, container: &[u8]) -> Vec<String> {
    let mut emails = Vec::new();
    let container_str = std::str::from_utf8(container).ok().unwrap_or("");
    let open = format!("<t:{}>", container_str);
    let close = format!("</t:{}>", container_str);

    if let Some(start) = xml.find(&open) {
        let end_pos = xml[start..].find(&close).map(|p| start + p).unwrap_or(xml.len());
        let inner = &xml[start..end_pos];

        // Find all <t:EmailAddress>...</t:EmailAddress> within
        let mut search_from = 0;
        while let Some(email_start) = inner[search_from..].find("<t:EmailAddress>") {
            let abs_start = search_from + email_start + "<t:EmailAddress>".len();
            if let Some(email_end) = inner[abs_start..].find("</t:EmailAddress>") {
                emails.push(inner[abs_start..abs_start + email_end].to_string());
                search_from = abs_start + email_end + "</t:EmailAddress>".len();
            } else {
                break;
            }
        }
    }
    emails
}

/// Render an EWS MessageType XML response for a created/sent email.
///
/// Per MS-OXWSCORE, the response includes ItemId and ChangeKey.
pub fn render_ews_message_item_xml(server_id: &str, change_key: &str, msg: &EwsMessage) -> String {
    format!(
        r#"<t:Message><t:ItemId Id="{}" ChangeKey="{}" /><t:Subject>{}</t:Subject><t:Importance>{}</t:Importance></t:Message>"#,
        xml_escape(server_id),
        xml_escape(change_key),
        xml_escape(&msg.subject),
        xml_escape(&msg.importance),
    )
}

/// Render a JMAP email as an EWS MessageType XML element.
///
/// Converts JMAP email fields to EWS Message XML for GetItem/FindItem responses.
pub fn render_jmap_email_as_ews_message(
    email: &JmapEmail,
    server_id: &str,
    change_key: &str,
) -> String {
    let subject = email.subject.as_deref().unwrap_or("(No Subject)");
    let sender = email.from.as_ref().and_then(|v| v.first());
    let sender_name = sender.as_ref().and_then(|s| s.name.as_deref()).unwrap_or("");
    let sender_email = sender.as_ref().and_then(|s| s.email.as_deref()).unwrap_or("");

    let to_xml = email.to.as_ref().map(|recipients| {
        recipients.iter().map(|r| {
            let name = xml_escape(r.name.as_deref().unwrap_or(""));
            let addr = xml_escape(r.email.as_deref().unwrap_or(""));
            format!("<t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{addr}</t:EmailAddress></t:Mailbox>")
        }).collect::<String>()
    }).unwrap_or_default();

    let cc_xml = email.cc.as_ref().map(|recipients| {
        recipients.iter().map(|r| {
            let name = xml_escape(r.name.as_deref().unwrap_or(""));
            let addr = xml_escape(r.email.as_deref().unwrap_or(""));
            format!("<t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{addr}</t:EmailAddress></t:Mailbox>")
        }).collect::<String>()
    }).unwrap_or_default();

    let body_preview = email.preview.as_deref().unwrap_or("");
    let (body_text, is_html) = extract_jmap_body(email);
    let body_type = if is_html { "HTML" } else { "Text" };

    let received_at = email.received_at.as_deref().unwrap_or("");
    let sent_at = email.sent_at.as_deref().unwrap_or("");
    let has_attachment = email.has_attachment.unwrap_or(false);
    let is_read = email.keywords.as_ref().is_some_and(|k| k.contains_key("$seen"));

    format!(
        r#"<t:Message><t:ItemId Id="{server_id}" ChangeKey="{change_key}" /><t:Subject>{subject}</t:Subject><t:Sender><t:Mailbox><t:Name>{sender_name}</t:Name><t:EmailAddress>{sender_email}</t:EmailAddress></t:Mailbox></t:Sender><t:ToRecipients>{to_xml}</t:ToRecipients><t:CcRecipients>{cc_xml}</t:CcRecipients><t:DateTimeReceived>{received_at}</t:DateTimeReceived><t:DateTimeSent>{sent_at}</t:DateTimeSent><t:IsRead>{is_read}</t:IsRead><t:HasAttachments>{has_attachment}</t:HasAttachments><t:Preview>{body_preview}</t:Preview><t:Body BodyType="{body_type}">{body_text}</t:Body></t:Message>"#,
        server_id = xml_escape(server_id),
        change_key = xml_escape(change_key),
        subject = xml_escape(subject),
        sender_name = xml_escape(sender_name),
        sender_email = xml_escape(sender_email),
        received_at = xml_escape(received_at),
        sent_at = xml_escape(sent_at),
        is_read = is_read,
        has_attachment = has_attachment,
        body_preview = xml_escape(body_preview),
        body_text = xml_escape(body_text),
    )
}

/// Render a JMAP email as an EAS ApplicationData XML element.
///
/// Per MS-ASEMAIL §2.2, the Email class includes elements like
/// Subject, From, To, Body, etc.
pub fn render_jmap_email_as_eas_application_data(
    email: &JmapEmail,
    server_id: &str,
    _collection_id: &str,
) -> String {
    let subject = email.subject.as_deref().unwrap_or("");
    let sender = email.from.as_ref().and_then(|v| v.first());
    let sender_name: &str = sender.as_ref().and_then(|s| s.name.as_deref()).unwrap_or("");
    let sender_email: &str = sender.as_ref().and_then(|s| s.email.as_deref()).unwrap_or("");

    let to_xml = email.to.as_ref().map(|recipients| {
        recipients.iter().map(|r| {
            let name = xml_escape(r.name.as_deref().unwrap_or(""));
            let addr = xml_escape(r.email.as_deref().unwrap_or(""));
            format!("<Email2:To><Email2:Name>{name}</Email2:Name><Email2:EmailAddress>{addr}</Email2:EmailAddress></Email2:To>")
        }).collect::<String>()
    }).unwrap_or_default();

    let (body_text, is_html) = extract_jmap_body(email);
    // Per MS-ASAIRS §2.2.2.6, Type values: 1=plain text, 2=HTML
    let body_type_num = if is_html { "2" } else { "1" };

    let received_at = email.received_at.as_deref().unwrap_or("");
    let is_read = email.keywords.as_ref().is_some_and(|k| k.contains_key("$seen"));
    let has_attachment = email.has_attachment.unwrap_or(false);
    let importance = email.keywords.as_ref().map_or("1", |k| {
        if k.contains_key("$important") { "2" } else { "1" }
    });

    format!(
        r#"<AirSync:ApplicationData><AirSync:ServerId>{server_id}</AirSync:ServerId><Email:Subject>{subject}</Email:Subject><Email:From>{sender_name} &lt;{sender_email}&gt;</Email:From>{to_xml}<Email:DateReceived>{received_at}</Email:DateReceived><Email:Importance>{importance}</Email:Importance><Email:Read>{is_read_int}</Email:Read><Email:HasAttachment>{has_attachment_int}</Email:HasAttachment><AirSyncBase:Body><AirSyncBase:Type>{body_type_num}</AirSyncBase:Type><AirSyncBase:Data>{body_text}</AirSyncBase:Data></AirSyncBase:Body></AirSync:ApplicationData>"#,
        server_id = xml_escape(server_id),
        subject = xml_escape(subject),
        sender_name = xml_escape(sender_name),
        sender_email = xml_escape(sender_email),
        received_at = xml_escape(received_at),
        importance = importance,
        is_read_int = if is_read { "1" } else { "0" },
        has_attachment_int = if has_attachment { "1" } else { "0" },
        body_text = xml_escape(body_text),
    )
}

/// Send an email on behalf of a user.
///
/// Prefers JMAP EmailSubmission/set (RFC 8621 §2.7) when available,
/// falling back to SMTP when JMAP submission is not configured.
/// Used by both EWS SendItem/CreateItem and EAS SendMail.
pub async fn send_email(
    state: &Arc<AppState>,
    msg: &EwsMessage,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<String> {
    // Prefer JMAP EmailSubmission when available
    if let Some(jmap) = &state.jmap_client {
        match send_email_jmap(state, jmap, msg, username, password).await {
            Ok(id) => return Ok(id),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "JMAP EmailSubmission failed, falling back to SMTP"
                );
            }
        }
    }

    // Fallback to SMTP
    send_email_smtp(state, msg, username, password).await
}

/// Send an email via JMAP EmailSubmission/set (RFC 8621 §2.7).
async fn send_email_jmap(
    state: &Arc<AppState>,
    jmap: &Arc<crate::jmap::JmapClient>,
    msg: &EwsMessage,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<String> {
    let account_id = jmap.get_account_id(username, password).await?;

    let from = if msg.from.is_empty() {
        format!(
            "{}@{}",
            username.split('@').next().unwrap_or(username),
            state.cfg.mail_domain
        )
    } else {
        msg.from.clone()
    };

    let html_body = if msg.body_type.eq_ignore_ascii_case("HTML") {
        Some(msg.body.as_str())
    } else {
        None
    };

    let email_id = jmap
        .submit_email(crate::jmap::SubmitEmailParams {
            account_id: &account_id,
            from: &from,
            to: &msg.to_recipients,
            cc: &msg.cc_recipients,
            bcc: &msg.bcc_recipients,
            subject: &msg.subject,
            text_body: &msg.body,
            html_body,
            username,
            password,
        })
        .await?;

    info!(
        target: "email",
        from = %from,
        to_count = msg.to_recipients.len(),
        subject_len = msg.subject.len(),
        email_id = %email_id,
        "Email sent via JMAP EmailSubmission"
    );

    Ok(email_id)
}

/// Send an email via SMTP on behalf of a user.
///
/// Used as fallback when JMAP submission is unavailable.
pub async fn send_email_smtp(
    state: &Arc<AppState>,
    msg: &EwsMessage,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<String> {
    let smtp = state.smtp_client.as_ref().ok_or_else(|| {
        anyhow::anyhow!(
            "Neither JMAP submission nor SMTP is configured; email sending is unavailable"
        )
    })?;

    let from = if msg.from.is_empty() {
        format!("{}@{}", username.split('@').next().unwrap_or(username), state.cfg.mail_domain)
    } else {
        msg.from.clone()
    };

    let html_body = if msg.body_type.eq_ignore_ascii_case("HTML") {
        Some(msg.body.as_str())
    } else {
        None
    };

    let params = crate::smtp::SendEmailParams {
        from: &from,
        to: msg.to_recipients.clone(),
        cc: msg.cc_recipients.clone(),
        bcc: msg.bcc_recipients.clone(),
        subject: &msg.subject,
        text_body: &msg.body,
        html_body,
        username,
        password,
    };

    let result = smtp.send_email(params).await?;

    info!(
        target: "email",
        from = %from,
        to_count = msg.to_recipients.len(),
        subject_len = msg.subject.len(),
        message_id = %result.message_id,
        "Email sent via SMTP"
    );

    Ok(result.message_id)
}

/// Fetch emails from JMAP for a specific folder.
///
/// Used by EWS FindItem/GetItem and EAS Sync for email folders.
/// Returns the full [`EmailListResult`] including the Email data type `state`
/// token for subsequent `Email/changes` calls.
pub async fn fetch_emails_jmap(
    state: &Arc<AppState>,
    account_id: &str,
    mailbox_role: &str,
    position: u64,
    limit: u64,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<crate::jmap::EmailListResult> {
    let jmap = state.jmap_client.as_ref().ok_or_else(|| {
        anyhow::anyhow!("JMAP is not configured; email reading is unavailable")
    })?;

    // Map mailbox role to JMAP filter
    let filter = match mailbox_role.to_lowercase().as_str() {
        "inbox" => Some(serde_json::json!({
            "inMailboxRole": "inbox"
        })),
        "sentitems" | "sent" => Some(serde_json::json!({
            "inMailboxRole": "sent"
        })),
        "drafts" | "draft" => Some(serde_json::json!({
            "inMailboxRole": "drafts"
        })),
        "junkemail" | "junk" => Some(serde_json::json!({
            "inMailboxRole": "junk"
        })),
        "deleteditems" | "trash" => Some(serde_json::json!({
            "inMailboxRole": "trash"
        })),
        _ => None, // All mailboxes
    };

    let result = jmap
        .query_emails(crate::jmap::QueryEmailsParams {
            account_id,
            filter,
            sort: None,
            position,
            limit,
            username,
            password,
        })
        .await?;

    Ok(result)
}

/// EAS folder type constants per MS-ASCMD §2.2.3.41.
/// These map to the Type element in FolderSync responses.
pub mod eas_folder_type {
    pub const CALENDAR: u8 = 8;
    pub const CONTACTS: u8 = 9;
    pub const EMAIL: u8 = 2;
    pub const TASKS: u8 = 7;
    pub const NOTES: u8 = 10;
    pub const JOURNAL: u8 = 11;
    // Generic folder types
    pub const INBOX: u8 = 2;
    pub const DRAFTS: u8 = 3;
    pub const SENT_ITEMS: u8 = 4;
    pub const DELETED_ITEMS: u8 = 5;
    pub const OUTBOX: u8 = 6;
    pub const JUNK_EMAIL: u8 = 7;
}

/// EAS FolderSync folder entries for email folders.
///
/// Per MS-ASCON §2.2.3.41.1, the Type element indicates the content class.
/// These folders are returned alongside the Calendar folder in FolderSync.
pub fn eas_email_folders_xml() -> String {
    [
        ("2", "0", "Inbox", eas_folder_type::INBOX),
        ("3", "0", "Drafts", eas_folder_type::DRAFTS),
        ("4", "0", "Sent Items", eas_folder_type::SENT_ITEMS),
        ("5", "0", "Deleted Items", eas_folder_type::DELETED_ITEMS),
        ("6", "0", "Outbox", eas_folder_type::OUTBOX),
        ("7", "0", "Junk Email", eas_folder_type::JUNK_EMAIL),
    ]
    .iter()
    .map(|(id, parent_id, display_name, folder_type)| {
        format!(
            r#"<Add><ServerId>{}</ServerId><ParentId>{}</ParentId><DisplayName>{}</DisplayName><Type>{}</Type></Add>"#,
            id, parent_id, display_name, folder_type
        )
    })
    .collect()
}

/// Extract text content from the first occurrence of an XML tag.
/// Simple helper for EAS XML parsing - not namespace-aware.
fn extract_first_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let _tag_str = std::str::from_utf8(tag).ok()?;
    let open_tag = format!("<{}>", std::str::from_utf8(tag).unwrap_or(""));
    let close_tag = format!("</{}>", std::str::from_utf8(tag).unwrap_or(""));
    if let Some(start) = xml.find(&open_tag) {
        let content_start = start + open_tag.len();
        if let Some(end) = xml[content_start..].find(&close_tag) {
            return Some(xml[content_start..content_start + end].to_string());
        }
    }
    None
}

/// Parse EAS SendMail request to extract MIME message and recipients.
///
/// Per MS-ASCMD §2.2.1.17, SendMail contains a ClientId and either
/// a MIME message or individual email elements.
pub fn parse_eas_sendmail(xml: &str) -> Option<EasSendMailRequest> {
    let client_id = extract_first_tag_text(xml, b"ClientId");
    let mime_data = extract_first_tag_text(xml, b"MIMEData");
    let save_in_sent = xml.contains("<SaveInSentItems")
        || xml.contains(":SaveInSentItems");

    Some(EasSendMailRequest {
        client_id,
        mime_data,
        save_in_sent,
    })
}

/// EAS SendMail request structure.
#[derive(Clone, Debug, Default)]
pub struct EasSendMailRequest {
    pub client_id: Option<String>,
    pub mime_data: Option<String>,
    pub save_in_sent: bool,
}

/// Prefix for email item IDs to distinguish them from calendar HMAC-based IDs.
///
/// Calendar items use HMAC-SHA256 server IDs (base64, no prefix). Email items
/// use the `"em-"` prefix followed by the raw JMAP email ID, making the mapping
/// reversible without a database lookup. This is essential because EWS GetItem,
/// UpdateItem, and DeleteItem receive the gateway's server ID from the client
/// and must translate it back to a JMAP ID to query the JMAP server.
pub const EMAIL_ID_PREFIX: &str = "em-";

/// Create a reversible email server ID from a JMAP email ID.
///
/// Format: `"em-{jmap_id}"` (e.g. `"em-Mbe4a2b"`).
///
/// The `"em-"` prefix distinguishes email IDs from calendar HMAC IDs and
/// allows reversing the mapping via [`jmap_id_from_email_server_id`].
pub fn email_server_id_from_jmap_id(jmap_id: &str) -> String {
    format!("{}{}", EMAIL_ID_PREFIX, jmap_id)
}

/// Create a reversible email server ID from a send result.
///
/// `send_email()` returns either a JMAP email ID (e.g. `"e50"`) or an RFC 5322
/// Message-ID (e.g. `"<1717012345.abc@host>"`). This function normalises both
/// into a valid gateway server ID:
///
/// - **JMAP ID**: passed through to [`email_server_id_from_jmap_id`] → `"em-e50"`.
/// - **RFC 5322 Message-ID**: angle brackets are stripped, yielding
///   `"em-1717012345.abc@host"`. The bare Message-ID (without `<>`) is a valid
///   opaque identifier per RFC 5322 §3.6.4 and is safe for embedding in XML.
pub fn email_server_id_from_send_result(id: &str) -> String {
    // Strip RFC 5322 angle-bracket delimiters if present.
    // A JMAP email ID never contains '<' or '>', so this is a no-op for JMAP.
    let stripped = id.trim_start_matches('<').trim_end_matches('>');
    format!("{}{}", EMAIL_ID_PREFIX, stripped)
}

/// Extract the JMAP email ID from a gateway email server ID.
///
/// Returns `None` if the ID doesn't have the `"em-"` prefix (e.g. it's a
/// calendar HMAC ID or malformed).
pub fn jmap_id_from_email_server_id(server_id: &str) -> Option<&str> {
    server_id.strip_prefix(EMAIL_ID_PREFIX)
}

/// Check whether a server ID belongs to an email item (has `"em-"` prefix).
pub fn is_email_server_id(id: &str) -> bool {
    id.starts_with(EMAIL_ID_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ews_message_basic() {
        let xml = r#"
        <t:Message>
            <t:Subject>Test Email</t:Subject>
            <t:Body BodyType="Text">Hello World</t:Body>
            <t:ToRecipients>
                <t:Mailbox><t:EmailAddress>user@example.com</t:EmailAddress></t:Mailbox>
            </t:ToRecipients>
            <t:Importance>Normal</t:Importance>
        </t:Message>"#;

        let msg = parse_ews_message(xml).expect("Should parse message");
        assert_eq!(msg.subject, "Test Email");
        assert_eq!(msg.body, "Hello World");
        assert_eq!(msg.body_type, "Text");
        assert_eq!(msg.to_recipients, vec!["user@example.com"]);
        assert_eq!(msg.importance, "Normal");
    }

    #[test]
    fn test_parse_ews_message_with_cc_bcc() {
        let xml = r#"
        <t:Message>
            <t:Subject>Multi-recipient</t:Subject>
            <t:ToRecipients>
                <t:Mailbox><t:EmailAddress>to1@example.com</t:EmailAddress></t:Mailbox>
                <t:Mailbox><t:EmailAddress>to2@example.com</t:EmailAddress></t:Mailbox>
            </t:ToRecipients>
            <t:CcRecipients>
                <t:Mailbox><t:EmailAddress>cc@example.com</t:EmailAddress></t:Mailbox>
            </t:CcRecipients>
            <t:BccRecipients>
                <t:Mailbox><t:EmailAddress>bcc@example.com</t:EmailAddress></t:Mailbox>
            </t:BccRecipients>
        </t:Message>"#;

        let msg = parse_ews_message(xml).expect("Should parse message");
        assert_eq!(msg.to_recipients.len(), 2);
        assert_eq!(msg.cc_recipients.len(), 1);
        assert_eq!(msg.bcc_recipients.len(), 1);
    }

    #[test]
    fn test_parse_ews_message_no_subject() {
        // Per RFC 5322 §3.6.5, Subject is optional. A missing <t:Subject>
        // must not cause parse_ews_message to return None — that would make
        // CreateItem fall through to calendar handling (silent misdispatch)
        // or SendItem return a false error.
        let xml = r#"
<t:Message>
    <t:Body BodyType="Text">No subject email</t:Body>
    <t:ToRecipients>
        <t:Mailbox><t:EmailAddress>user@example.com</t:EmailAddress></t:Mailbox>
    </t:ToRecipients>
</t:Message>"#;

        let msg = parse_ews_message(xml).expect("Should parse message without Subject");
        assert_eq!(msg.subject, "");
        assert_eq!(msg.body, "No subject email");
        assert_eq!(msg.to_recipients, vec!["user@example.com"]);
    }

    #[test]
    fn test_message_disposition_parsing() {
        assert_eq!(MessageDisposition::parse("SaveOnly"), Some(MessageDisposition::SaveOnly));
        assert_eq!(MessageDisposition::parse("SendOnly"), Some(MessageDisposition::SendOnly));
        assert_eq!(MessageDisposition::parse("SendAndSaveCopy"), Some(MessageDisposition::SendAndSaveCopy));
        assert_eq!(MessageDisposition::parse("Invalid"), None);
    }

    #[test]
    fn test_render_ews_message_item_xml() {
        let msg = EwsMessage { subject: "Hello".to_string(), importance: "Normal".to_string(), ..Default::default() };
        let xml = render_ews_message_item_xml("id123", "ck456", &msg);
        assert!(xml.contains("id123"));
        assert!(xml.contains("ck456"));
        assert!(xml.contains("Hello"));
        assert!(xml.contains("<t:Message>"));
    }

    #[test]
    fn test_email_server_id_round_trip() {
        let jmap_id = "Mbe4a2b";
        let server_id = email_server_id_from_jmap_id(jmap_id);
        assert!(server_id.starts_with("em-"), "Must have em- prefix");
        assert_eq!(jmap_id_from_email_server_id(&server_id), Some(jmap_id));
        assert!(is_email_server_id(&server_id));
    }

    #[test]
    fn test_email_server_id_deterministic() {
        let a = email_server_id_from_jmap_id("email-1");
        let b = email_server_id_from_jmap_id("email-1");
        assert_eq!(a, b, "Same inputs must produce same server ID");

        let c = email_server_id_from_jmap_id("email-2");
        assert_ne!(a, c, "Different inputs must produce different server IDs");
    }

    #[test]
    fn test_jmap_id_from_non_email_server_id() {
        // Calendar HMAC IDs don't have the em- prefix
        assert_eq!(jmap_id_from_email_server_id("abc123xyz"), None);
        assert!(!is_email_server_id("abc123xyz"));
        assert!(!is_email_server_id(""));
    }

    #[test]
    fn test_email_server_id_from_send_result_jmap() {
        // JMAP IDs pass through without angle brackets
        let server_id = email_server_id_from_send_result("e50");
        assert_eq!(server_id, "em-e50");
        assert!(is_email_server_id(&server_id));
    }

    #[test]
    fn test_email_server_id_from_send_result_smtp() {
        // RFC 5322 Message-IDs have angle brackets stripped
        let server_id = email_server_id_from_send_result("<1717012345.abc@host>");
        assert_eq!(server_id, "em-1717012345.abc@host");
        assert!(is_email_server_id(&server_id));
        // Must not contain raw angle brackets (breaks XML)
        assert!(!server_id.contains('<'));
        assert!(!server_id.contains('>'));
    }

    #[test]
    fn test_eas_email_folders_xml_contains_inbox() {
        let xml = eas_email_folders_xml();
        assert!(xml.contains("Inbox"), "Must include Inbox folder");
        assert!(xml.contains("Sent Items"), "Must include Sent Items folder");
        assert!(xml.contains("Drafts"), "Must include Drafts folder");
        assert!(xml.contains("Junk Email"), "Must include Junk Email folder");
    }

    #[test]
    fn test_extract_jmap_body_prefers_text_over_html() {
        use crate::jmap::{JmapBodyPart, JmapBodyValue, JmapEmail};
        use std::collections::HashMap;

        let email = JmapEmail {
            id: Some("test".to_string()),
            text_body: Some(vec![JmapBodyPart {
                part_id: "text-part".to_string(),
                blob_id: None,
                size: None,
                content_type: Some("text/plain".to_string()),
                charset: None,
            }]),
            html_body: Some(vec![JmapBodyPart {
                part_id: "html-part".to_string(),
                blob_id: None,
                size: None,
                content_type: Some("text/html".to_string()),
                charset: None,
            }]),
            body_values: Some(HashMap::from([
                ("text-part".to_string(), JmapBodyValue { value: "Plain text body".to_string(), is_encoding_problem: None }),
                ("html-part".to_string(), JmapBodyValue { value: "<p>HTML body</p>".to_string(), is_encoding_problem: None }),
            ])),
            ..Default::default()
        };

        let (body, is_html) = extract_jmap_body(&email);
        assert_eq!(body, "Plain text body");
        assert!(!is_html, "Should prefer plain text over HTML");
    }

    #[test]
    fn test_extract_jmap_body_falls_back_to_html() {
        use crate::jmap::{JmapBodyPart, JmapBodyValue, JmapEmail};
        use std::collections::HashMap;

        let email = JmapEmail {
            id: Some("test".to_string()),
            text_body: None,
            html_body: Some(vec![JmapBodyPart {
                part_id: "html-part".to_string(),
                blob_id: None,
                size: None,
                content_type: Some("text/html".to_string()),
                charset: None,
            }]),
            body_values: Some(HashMap::from([
                ("html-part".to_string(), JmapBodyValue { value: "<p>HTML only</p>".to_string(), is_encoding_problem: None }),
            ])),
            ..Default::default()
        };

        let (body, is_html) = extract_jmap_body(&email);
        assert_eq!(body, "<p>HTML only</p>");
        assert!(is_html, "Should report HTML when falling back to htmlBody");
    }

    #[test]
    fn test_extract_jmap_body_empty_when_no_bodies() {
        use crate::jmap::JmapEmail;

        let email = JmapEmail {
            id: Some("test".to_string()),
            body_values: None,
            ..Default::default()
        };

        let (body, is_html) = extract_jmap_body(&email);
        assert_eq!(body, "");
        assert!(!is_html);
    }
}