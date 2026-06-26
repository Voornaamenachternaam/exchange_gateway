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
use anyhow::anyhow;
use secrecy::SecretString;
use serde_json::json;
use std::sync::Arc;
use tracing::info;

/// Compute the email's display date for EWS/EAS clients.
///
/// Prefer `received_at` to reflect the actual arrival time. If missing, fall back to `sent_at`.
/// This fallback avoids epoch "01/01/1970" dates that occur when `receivedAt` is unset.
///
/// Note: Using `sent_at` as a fallback misrepresents the true receive time in some cases
/// (e.g., delayed delivery). However, it provides a reasonable approximation and prevents
/// clients from showing nonsensical epoch dates. This behavior is expected by clients.
fn compute_email_date_received(email: &JmapEmail) -> Option<&str> {
    email.received_at.as_deref().or(email.sent_at.as_deref())
}

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
    // Try bodyValues if present (preferred source)
    if let Some(bv) = email.body_values.as_ref() {
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
    }
    // Last resort: use preview (plain text snippet) to avoid empty body
    if let Some(preview) = email.preview.as_ref() {
        return (preview.as_str(), false);
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
            body_content = unescape_xml_text(&body[content_start..content_start + end]);
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
    let importance =
        extract_ews_tag_text(body, b"Importance").unwrap_or_else(|| "Normal".to_string());

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

/// Extract text content from an EWS XML tag, unescaping XML entities.
///
/// Per the XML specification, predefined entities `&amp;`, `&lt;`, `&gt;`,
/// `&quot;`, and `&apos;` must be resolved. Without unescaping, sent emails
/// contain raw entities like `&amp;` instead of `&`.
fn extract_ews_tag_text(xml: &str, tag: &[u8]) -> Option<String> {
    let open = format!("<t:{}>", std::str::from_utf8(tag).ok()?);
    let close = format!("</t:{}>", std::str::from_utf8(tag).ok()?);

    if let Some(start) = xml.find(&open) {
        let content_start = start + open.len();
        if let Some(end) = xml[content_start..].find(&close) {
            let raw = &xml[content_start..content_start + end];
            return Some(unescape_xml_text(raw));
        }
    }
    None
}

/// Unescape XML predefined entities in text content.
///
/// Uses `quick_xml::escape::unescape()` to resolve `&amp;`, `&lt;`, `&gt;`,
/// `&quot;`, `&apos;`, and numeric character references. On unescape failure
/// (malformed entity), returns the original text unchanged — this is safer
/// than dropping the email entirely.
fn unescape_xml_text(raw: &str) -> String {
    match quick_xml::escape::unescape(raw) {
        Ok(cow) => cow.into_owned(),
        Err(e) => {
            tracing::warn!(error = %e, text = raw, "XML entity unescape failed; using raw text");
            raw.to_string()
        }
    }
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
                let raw = &rest[email_content_start..email_content_start + email_end];
                return Some(unescape_xml_text(raw));
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
        let end_pos = xml[start..]
            .find(&close)
            .map(|p| start + p)
            .unwrap_or(xml.len());
        let inner = &xml[start..end_pos];

        // Find all <t:EmailAddress>...</t:EmailAddress> within
        let mut search_from = 0;
        while let Some(email_start) = inner[search_from..].find("<t:EmailAddress>") {
            let abs_start = search_from + email_start + "<t:EmailAddress>".len();
            if let Some(email_end) = inner[abs_start..].find("</t:EmailAddress>") {
                let raw = &inner[abs_start..abs_start + email_end];
                emails.push(unescape_xml_text(raw));
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
    let subject = sanitize_for_xml(email.subject.as_deref().unwrap_or("(No Subject)"));
    let sender = email.from.as_ref().and_then(|v| v.first());
    let sender_name = sanitize_for_xml(
        sender
            .as_ref()
            .and_then(|s| s.name.as_deref())
            .unwrap_or(""),
    );
    let sender_email = sanitize_for_xml(
        sender
            .as_ref()
            .and_then(|s| s.email.as_deref())
            .unwrap_or(""),
    );

    let to_xml = email.to.as_ref().map(|recipients| {
        recipients.iter().map(|r| {
            let name_san = sanitize_for_xml(r.name.as_deref().unwrap_or(""));
            let addr_san = sanitize_for_xml(r.email.as_deref().unwrap_or(""));
            let name = xml_escape(&name_san);
            let addr = xml_escape(&addr_san);
            format!("<t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{addr}</t:EmailAddress></t:Mailbox>")
        }).collect::<String>()
    }).unwrap_or_default();

    let cc_xml = email.cc.as_ref().map(|recipients| {
        recipients.iter().map(|r| {
            let name_san = sanitize_for_xml(r.name.as_deref().unwrap_or(""));
            let addr_san = sanitize_for_xml(r.email.as_deref().unwrap_or(""));
            let name = xml_escape(&name_san);
            let addr = xml_escape(&addr_san);
            format!("<t:Mailbox><t:Name>{name}</t:Name><t:EmailAddress>{addr}</t:EmailAddress></t:Mailbox>")
        }).collect::<String>()
    }).unwrap_or_default();

    let body_preview = sanitize_for_xml(email.preview.as_deref().unwrap_or(""));
    let (body_text_raw, is_html) = extract_jmap_body(email);
    let body_text_sanitized = sanitize_for_xml(body_text_raw);
    let body_type = if is_html { "HTML" } else { "Text" };

    let received_at_raw = compute_email_date_received(email).unwrap_or("");
    let received_at = sanitize_for_xml(received_at_raw);
    let sent_at = sanitize_for_xml(email.sent_at.as_deref().unwrap_or(""));
    let has_attachment = email.has_attachment.unwrap_or(false);
    let is_read = email
        .keywords
        .as_ref()
        .is_some_and(|k| k.contains_key("$seen"));

    format!(
        r#"<t:Message><t:ItemId Id="{server_id}" ChangeKey="{change_key}" /><t:Subject>{subject}</t:Subject><t:Sender><t:Mailbox><t:Name>{sender_name}</t:Name><t:EmailAddress>{sender_email}</t:EmailAddress></t:Mailbox></t:Sender><t:ToRecipients>{to_xml}</t:ToRecipients><t:CcRecipients>{cc_xml}</t:CcRecipients><t:DateTimeReceived>{received_at}</t:DateTimeReceived><t:DateTimeSent>{sent_at}</t:DateTimeSent><t:IsRead>{is_read}</t:IsRead><t:HasAttachments>{has_attachment}</t:HasAttachments><t:Preview>{body_preview}</t:Preview><t:Body BodyType="{body_type}">{body_text}</t:Body></t:Message>"#,
        server_id = xml_escape(server_id),
        change_key = xml_escape(change_key),
        subject = xml_escape(&subject),
        sender_name = xml_escape(&sender_name),
        sender_email = xml_escape(&sender_email),
        received_at = xml_escape(&received_at),
        sent_at = xml_escape(&sent_at),
        is_read = is_read,
        has_attachment = has_attachment,
        body_preview = xml_escape(&body_preview),
        body_text = xml_escape(&body_text_sanitized),
    )
}

/// Render a JMAP email as an EAS ApplicationData XML element.
///
/// Per MS-ASEMAIL §2.2, the Email class includes elements like
/// Subject, From, To, Body, etc.
/// Remove characters that are invalid in XML 1.0.
///
/// XML 1.0 allowed characters: #x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] | [#x10000-#x10FFFF]
fn sanitize_for_xml(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            matches!(
                c,
                '\t' | '\n'
                    | '\r'
                    | (' '..='\u{D7FF}')
                    | ('\u{E000}'..='\u{FFFD}')
                    | ('\u{10000}'..='\u{10FFFF}')
            )
        })
        .collect()
}

pub fn render_jmap_email_as_eas_application_data(
    email: &JmapEmail,
    server_id: &str,
    _collection_id: &str,
) -> String {
    let subject = sanitize_for_xml(email.subject.as_deref().unwrap_or(""));
    let sender = email.from.as_ref().and_then(|v| v.first());
    let sender_name_raw = sender
        .as_ref()
        .and_then(|s| s.name.as_deref())
        .unwrap_or("");
    let sender_name = sanitize_for_xml(sender_name_raw);
    let sender_email_raw = sender
        .as_ref()
        .and_then(|s| s.email.as_deref())
        .unwrap_or("");
    let sender_email = sanitize_for_xml(sender_email_raw);

    // Build a single Email:To element with recipients formatted as "Name <email>" separated by semicolons.
    // Filter out recipients without an email address. Sanitize and escape each part.
    let to_xml = if let Some(recipients) = email.to.as_ref()
        && !recipients.is_empty()
    {
        let mut to_parts: Vec<String> = Vec::new();
        for r in recipients {
            let addr = r.email.as_deref().unwrap_or("");
            if addr.is_empty() {
                continue; // Skip recipients without an email address
            }
            let name = r.name.as_deref().unwrap_or("");
            let sanitized_name = sanitize_for_xml(name);
            let escaped_name = xml_escape(&sanitized_name);
            let sanitized_addr = sanitize_for_xml(addr);
            let escaped_addr = xml_escape(&sanitized_addr);
            if !escaped_name.is_empty() {
                to_parts.push(format!("{} &lt;{}&gt;", escaped_name, escaped_addr));
            } else {
                to_parts.push(escaped_addr.to_string());
            }
        }
        if to_parts.is_empty() {
            String::new()
        } else {
            format!("<Email:To>{}</Email:To>", to_parts.join("; "))
        }
    } else {
        String::new()
    };

    let (body_text_raw, is_html) = extract_jmap_body(email);
    let body_text_sanitized = sanitize_for_xml(body_text_raw);
    // Per MS-ASAIRS §2.2.2.6, Type values: 1=plain text, 2=HTML
    let body_type_num = if is_html { "2" } else { "1" };

    let received_at_raw = compute_email_date_received(email).unwrap_or("");
    let received_at = sanitize_for_xml(received_at_raw);
    let is_read = email
        .keywords
        .as_ref()
        .is_some_and(|k| k.contains_key("$seen"));
    let has_attachment = email.has_attachment.unwrap_or(false);
    let importance = email.keywords.as_ref().map_or("1", |k| {
        if k.contains_key("$important") {
            "2"
        } else {
            "1"
        }
    });

    format!(
        r#"<ApplicationData><ServerId>{server_id}</ServerId><Email:Subject>{subject}</Email:Subject><Email:From>{sender_name} &lt;{sender_email}&gt;</Email:From>{to_xml}<Email:DateReceived>{received_at}</Email:DateReceived><Email:Importance>{importance}</Email:Importance><Email:Read>{is_read_int}</Email:Read><Email:HasAttachments>{has_attachment_int}</Email:HasAttachments><AirSyncBase:Body><AirSyncBase:Type>{body_type_num}</AirSyncBase:Type><AirSyncBase:Data>{body_text}</AirSyncBase:Data></AirSyncBase:Body></ApplicationData>"#,
        server_id = xml_escape(server_id),
        subject = xml_escape(&subject),
        sender_name = xml_escape(&sender_name),
        sender_email = xml_escape(&sender_email),
        received_at = xml_escape(&received_at),
        importance = importance,
        is_read_int = if is_read { "1" } else { "0" },
        has_attachment_int = if has_attachment { "1" } else { "0" },
        body_text = xml_escape(&body_text_sanitized),
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

/// Save an email draft via JMAP Email/set.
///
/// Creates the message in the Drafts mailbox and stores the server_id mapping
/// in the local database. This is used for CreateItem with MessageDisposition
/// "SaveOnly" or "SendAndSaveCopy" (draft portion).
///
/// Returns the created server_id and change_key on success.
pub async fn save_draft_via_jmap(
    state: &Arc<AppState>,
    jmap: &Arc<crate::jmap::JmapClient>,
    msg: &EwsMessage,
    account_id: &str,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<(String, String)> {
    // Ensure the draft is placed in the Drafts mailbox.
    let draft_mailbox_ids = jmap
        .get_mailbox_ids_for_role(account_id, "drafts", username, password)
        .await
        .unwrap_or_else(|_| vec!["drafts".to_string()]);

    if draft_mailbox_ids.is_empty() {
        return Err(anyhow!("No Drafts mailbox found"));
    }

    // Build the email object for the draft.
    // Note: For drafts, we store the full MIME content to preserve formatting.
    // The JMAP server will parse and store the email normally.
    let mut email_obj = json!({
        "mailboxIds": {},
    });

    // Set mailboxIds to Drafts
    for mb_id in &draft_mailbox_ids {
        email_obj["mailboxIds"][mb_id] = json!(true);
    }

    // Set From header
    let from = if msg.from.is_empty() {
        format!(
            "{}@{}",
            username.split('@').next().unwrap_or(username),
            state.cfg.mail_domain
        )
    } else {
        msg.from.clone()
    };
    email_obj["from"] = json!([{ "email": from }]);

    // Recipients
    if !msg.to_recipients.is_empty() {
        email_obj["to"] = json!(
            msg.to_recipients
                .iter()
                .map(|addr| json!({ "email": addr }))
                .collect::<Vec<_>>()
        );
    }
    if !msg.cc_recipients.is_empty() {
        email_obj["cc"] = json!(
            msg.cc_recipients
                .iter()
                .map(|addr| json!({ "email": addr }))
                .collect::<Vec<_>>()
        );
    }
    if !msg.bcc_recipients.is_empty() {
        email_obj["bcc"] = json!(
            msg.bcc_recipients
                .iter()
                .map(|addr| json!({ "email": addr }))
                .collect::<Vec<_>>()
        );
    }

    // Subject
    if !msg.subject.is_empty() {
        email_obj["subject"] = json!(msg.subject);
    }

    // Body: Construct bodyValues and textBody/htmlBody per RFC 8621 §4.1.4
    let is_html = msg.body_type.eq_ignore_ascii_case("HTML");
    let mut body_values = json!({
        "text": {
            "value": msg.body,
            "type": "text/plain",
            "charset": "utf-8",
            "isEncodingProblem": false,
            "isTruncated": false,
        }
    });
    if is_html {
        body_values["html"] = json!({
            "value": msg.body,
            "type": "text/html",
            "charset": "utf-8",
            "isEncodingProblem": false,
            "isTruncated": false,
        });
    }
    email_obj["bodyValues"] = body_values;
    email_obj["textBody"] = json!([{ "partId": "text", "type": "text/plain" }]);
    if is_html {
        email_obj["htmlBody"] = json!([{ "partId": "html", "type": "text/html" }]);
    }

    // Add keywords if present (like Draft)
    // We could set "draft" keyword, but JMAP may auto-set based on mailbox.
    // Let's not force it; the server may set it automatically for Drafts mailbox.

    let mut method_calls = vec![(
        "Email/set",
        json!({
            "accountId": account_id,
            "create": {
                "draft0": email_obj,
            },
        }),
        "cs0",
    )];

    // Also optionally fetch the created ID
    method_calls.push((
        "Email/get",
        json!({
            "accountId": account_id,
            "ids": ["#draft0"],
        }),
        "cg0",
    ));

    let session = jmap.get_session(username, password).await?;
    let api_url = session.api_url.as_str();

    let response = jmap
        .api_call(
            api_url,
            &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            method_calls,
            username,
            password,
        )
        .await?;

    let mut created_id: Option<String> = None;
    for (method, data, _) in response.method_responses {
        if method == "Email/set" {
            if let Some(created) = data.get("created").and_then(|v| v.as_object()) {
                if let Some(id) = created.get("draft0").and_then(|v| v.get("id")).and_then(|v| v.as_str()) {
                    created_id = Some(id.to_string());
                }
            }
        } else if method == "Email/get" {
            // We could extract more data if needed
        }
    }

    let jmap_id = created_id.ok_or_else(|| anyhow!("Failed to create draft via JMAP"))?;

    // Store mapping in local database
    let server_id = format!("em-{}", jmap_id);
    let owner = crate::util::normalize_email(username);

    // Insert into item_map if not exists; updates are idempotent.
    // Use the resource_href as jmap_id, and store uid if available (none for drafts).
    sqlx::query(
        r#"
        INSERT OR REPLACE INTO item_map (owner, server_id, resource_href, uid, caldav_href, etag, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, datetime('now'))
        "#,
    )
    .bind(&owner)
    .bind(&server_id)
    .bind(&jmap_id)
    .bind::<Option<&str>>(None) // uid not available for drafts
    .bind::<Option<&str>>(None) // caldav_href not used for email
    .bind::<Option<&str>>(None) // etag not used for email (JMAP state-based)
    .execute(state.storage.pool())
    .await?;

    info!(
        target: "email",
        %server_id,
        %jmap_id,
        %owner,
        "Draft saved via JMAP"
    );

    // Return server_id and change_key (change_key == server_id for email)
    Ok((server_id.clone(), server_id))
}

/// Move an email to a different folder via JMAP Email/set.
///
/// This implements MoveItem by updating the email's mailboxIds.
/// `server_id` is the gateway's server ID (em-<jmap_id>).
/// `target_folder` is the distinguished folder name (e.g., "inbox", "sent", "drafts", "trash").
pub async fn move_email_via_jmap(
    _state: &Arc<AppState>,
    jmap: &Arc<crate::jmap::JmapClient>,
    account_id: &str,
    server_id: &str,
    target_folder: &str,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<String> {
    // Extract the JMAP ID from the server_id
    let jmap_id = match crate::email::jmap_id_from_email_server_id(server_id) {
        Some(id) => id.to_string(),
        None => return Err(anyhow!("Invalid server ID format: {}", server_id)),
    };

    // Get the mailbox ID for the target folder role
    let target_mailbox_ids = jmap
        .get_mailbox_ids_for_role(account_id, target_folder, username, password)
        .await?;

    if target_mailbox_ids.is_empty() {
        return Err(anyhow!("Target folder '{}' not found", target_folder));
    }

    // For simplicity, use the first mailbox ID (servers may return multiple with same role)
    let target_mb_id = &target_mailbox_ids[0];

    // Build the update: set mailboxIds to contain only the target folder
    // This effectively moves the email (removes from other mailboxes)
    let update_patch = json!({
        "mailboxIds": { (target_mb_id): true },
    });

    jmap.update_email(
        account_id,
        &json!({
            (jmap_id): update_patch,
        }),
        username,
        password,
    )
    .await?;

    // Return a new change key; could be same as server_id for simplicity
    Ok(server_id.to_string())
}

/// Copy an email to another folder via JMAP Email/set.
///
/// This implements CopyItem by adding the target mailboxId without removing from the current mailbox(es).
/// `server_id` is the gateway's server ID (em-<jmap_id>).
/// `dest_folder` is the distinguished folder name (e.g., "inbox", "sent", "drafts", "trash").
pub async fn copy_email_via_jmap(
    _state: &Arc<AppState>,
    jmap: &Arc<crate::jmap::JmapClient>,
    account_id: &str,
    server_id: &str,
    dest_folder: &str,
    username: &str,
    password: &SecretString,
) -> anyhow::Result<String> {
    // Extract the JMAP ID from the server_id
    let jmap_id = match crate::email::jmap_id_from_email_server_id(server_id) {
        Some(id) => id.to_string(),
        None => return Err(anyhow!("Invalid server ID format: {}", server_id)),
    };

    // Get the mailbox ID for the destination folder role
    let dest_mailbox_ids = jmap
        .get_mailbox_ids_for_role(account_id, dest_folder, username, password)
        .await?;

    if dest_mailbox_ids.is_empty() {
        return Err(anyhow!("Destination folder '{}' not found", dest_folder));
    }

    // Use first mailbox ID for simplicity
    let dest_mb_id = &dest_mailbox_ids[0];

    // Build the update patch: add the destination mailboxId to the existing set.
    // We use "mailboxIds": { dest_mb_id: true } which does not remove existing ones
    // unless we explicitly clear them. Per JMAP, setting to true adds, false removes.
    let update_patch = json!({
        "mailboxIds": { (dest_mb_id): true },
    });

    jmap.update_email(
        account_id,
        &json!({
            (jmap_id): update_patch,
        }),
        username,
        password,
    )
    .await?;

    // Return same change key (since content unchanged)
    Ok(server_id.to_string())
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
    let jmap = state
        .jmap_client
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("JMAP is not configured; email reading is unavailable"))?;

    // Map mailbox role to JMAP filter.
    // "outbox" returns empty result — JMAP has no outbox role; outbound email
    // is handled via EmailSubmission/set. Return early with empty result.
    let mailbox_role_lower = mailbox_role.to_lowercase();
    if mailbox_role_lower == "outbox" {
        return Ok(crate::jmap::EmailListResult {
            emails: Vec::new(),
            total: 0,
            can_calculate_changes: false,
            query_state: String::new(),
            state: String::new(),
        });
    }

    // Normalize role to JMAP standard role name
    let normalized_role = match mailbox_role_lower.as_str() {
        "inbox" => "inbox",
        "sentitems" | "sent" => "sent",
        "drafts" | "draft" => "drafts",
        "junkemail" | "junk" => "junk",
        "deleteditems" | "trash" => "trash",
        "outbox" => {
            return Ok(crate::jmap::EmailListResult {
                emails: Vec::new(),
                total: 0,
                can_calculate_changes: false,
                query_state: String::new(),
                state: String::new(),
            });
        }
        _ => {
            tracing::warn!(role = %mailbox_role, "Unrecognised mailbox role; returning empty email list");
            return Ok(crate::jmap::EmailListResult {
                emails: Vec::new(),
                total: 0,
                can_calculate_changes: false,
                query_state: String::new(),
                state: String::new(),
            });
        }
    };

    // Fetch the mailbox ID(s) for the requested role using Mailbox/query.
    // Use the standard 'inMailbox' filter (not 'inMailboxRole') for maximum
    // compatibility across JMAP servers.
    match jmap
        .get_mailbox_ids_for_role(account_id, normalized_role, username, password)
        .await
    {
        Ok(mailbox_ids) if !mailbox_ids.is_empty() => {
            // RFC 8621 §4.3.1: "inMailbox" filter must be a single mailbox ID (String).
            // get_mailbox_ids_for_role returns a Vec for flexibility, but in practice
            // each role maps to exactly one mailbox. Use the first ID.
            if mailbox_ids.len() > 1 {
                tracing::warn!(
                    role = %normalized_role,
                    count = mailbox_ids.len(),
                    "Multiple mailbox IDs found for role; using first"
                );
            }
            let filter = Some(serde_json::json!({
                "inMailbox": mailbox_ids[0]
            }));
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
        Ok(_) => {
            // No mailbox for that role — return empty result.
            tracing::info!(role = %normalized_role, "No mailbox found for role; returning empty email list");
            Ok(crate::jmap::EmailListResult {
                emails: Vec::new(),
                total: 0,
                can_calculate_changes: false,
                query_state: String::new(),
                state: String::new(),
            })
        }
        Err(e) => {
            // Log the error but still return empty result to maintain sync stability.
            tracing::warn!(error = %e, role = %normalized_role, "Failed to fetch mailbox IDs for role");
            Ok(crate::jmap::EmailListResult {
                emails: Vec::new(),
                total: 0,
                can_calculate_changes: false,
                query_state: String::new(),
                state: String::new(),
            })
        }
    }
}

/// EAS folder type constants per MS-ASCMD §2.2.3.186.3.
/// These map to the Type element in FolderSync responses.
pub mod eas_folder_type {
    pub const CALENDAR: u8 = 8;
    pub const CONTACTS: u8 = 9;
    pub const EMAIL: u8 = 2; // Alias for INBOX
    pub const TASKS: u8 = 7;
    pub const NOTES: u8 = 10;
    pub const JOURNAL: u8 = 11;
    // Specific email folder types per MS-ASCMD §2.2.3.186.3
    pub const INBOX: u8 = 2; // Default Inbox folder
    pub const DRAFTS: u8 = 3; // Default Drafts folder
    pub const DELETED_ITEMS: u8 = 4; // Default Deleted Items folder
    pub const SENT_ITEMS: u8 = 5; // Default Sent Items folder
    pub const OUTBOX: u8 = 6; // Default Outbox folder
    // No dedicated Junk Email type in the spec — use 12 (User-created Mail folder)
    pub const JUNK_EMAIL: u8 = 12; // User-created Mail folder
}

/// Map an EAS CollectionId to the JMAP mailbox role used for filtering.
///
/// Returns `None` for the Outbox (CollectionId "6") and any unrecognised
/// CollectionId because JMAP has no outbox role and unknown IDs have no
/// JMAP mailbox mapping. Returning `None` signals the caller to return an
/// empty result rather than querying all mailboxes.
pub fn eas_collection_id_to_mailbox_role(collection_id: &str) -> Option<&'static str> {
    match collection_id {
        "2" => Some("inbox"),
        "3" => Some("drafts"),
        "4" => Some("trash"),
        "5" => Some("sent"),
        "6" => None, // Outbox — no JMAP equivalent; handled via EmailSubmission
        "12" => Some("junk"),
        _ => {
            tracing::warn!(
                collection_id,
                "Unrecognised EAS CollectionId; returning empty"
            );
            None
        }
    }
}

/// Returns true if the EAS CollectionId refers to an email folder.
///
/// Checks the known email CollectionIds directly so valid non-email folders (for
/// example, Calendar CollectionId "1") do not pass through the mailbox-role
/// mapper and emit unknown-ID warning logs. Used to route Sync requests when
/// `<Class>` is absent from the request.
pub fn is_eas_email_collection_id(collection_id: &str) -> bool {
    matches!(collection_id, "2" | "3" | "4" | "5" | "6" | "12")
}

/// EAS FolderSync folder entries for email folders.
///
/// Per MS-ASCON §2.2.3.41.1, the Type element indicates the content class.
/// These folders are returned alongside the Calendar folder in FolderSync.
pub fn eas_email_folders_xml() -> String {
    [
        ("2", "0", "Inbox", eas_folder_type::INBOX),
        ("3", "0", "Drafts", eas_folder_type::DRAFTS),
        ("4", "0", "Deleted Items", eas_folder_type::DELETED_ITEMS),
        ("5", "0", "Sent Items", eas_folder_type::SENT_ITEMS),
        ("6", "0", "Outbox", eas_folder_type::OUTBOX),
        ("12", "0", "Junk Email", eas_folder_type::JUNK_EMAIL),
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
    let save_in_sent = xml.contains("<SaveInSentItems") || xml.contains(":SaveInSentItems");

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
        assert_eq!(
            MessageDisposition::parse("SaveOnly"),
            Some(MessageDisposition::SaveOnly)
        );
        assert_eq!(
            MessageDisposition::parse("SendOnly"),
            Some(MessageDisposition::SendOnly)
        );
        assert_eq!(
            MessageDisposition::parse("SendAndSaveCopy"),
            Some(MessageDisposition::SendAndSaveCopy)
        );
        assert_eq!(MessageDisposition::parse("Invalid"), None);
    }

    #[test]
    fn test_render_ews_message_item_xml() {
        let msg = EwsMessage {
            subject: "Hello".to_string(),
            importance: "Normal".to_string(),
            ..Default::default()
        };
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
        assert!(
            xml.contains("<ServerId>12</ServerId>"),
            "Junk Email must use CollectionId 12"
        );
        assert!(
            !xml.contains("<ServerId>7</ServerId>"),
            "CollectionId 7 is Tasks, not Junk Email"
        );
    }

    #[test]
    fn test_eas_folder_type_values_per_ms_ascmd() {
        // Per MS-ASCMD §2.2.3.186.3
        assert_eq!(eas_folder_type::INBOX, 2, "Inbox = Default Inbox folder");
        assert_eq!(eas_folder_type::DRAFTS, 3, "Drafts = Default Drafts folder");
        assert_eq!(
            eas_folder_type::DELETED_ITEMS,
            4,
            "Deleted Items = Default Deleted Items folder"
        );
        assert_eq!(
            eas_folder_type::SENT_ITEMS,
            5,
            "Sent Items = Default Sent Items folder"
        );
        assert_eq!(eas_folder_type::OUTBOX, 6, "Outbox = Default Outbox folder");
        assert_eq!(
            eas_folder_type::JUNK_EMAIL,
            12,
            "Junk Email = User-created Mail folder"
        );
        assert_eq!(
            eas_folder_type::CALENDAR,
            8,
            "Calendar = Default Calendar folder"
        );
        assert_eq!(
            eas_folder_type::CONTACTS,
            9,
            "Contacts = Default Contacts folder"
        );
        assert_eq!(eas_folder_type::TASKS, 7, "Tasks = Default Tasks folder");
    }

    #[test]
    fn test_eas_email_folders_xml_type_values() {
        let xml = eas_email_folders_xml();
        // Verify correct Type values are emitted in the XML
        assert!(xml.contains("<Type>2</Type>"), "Inbox must have Type=2");
        assert!(xml.contains("<Type>3</Type>"), "Drafts must have Type=3");
        assert!(
            xml.contains("<Type>4</Type>"),
            "Deleted Items must have Type=4"
        );
        assert!(
            xml.contains("<Type>5</Type>"),
            "Sent Items must have Type=5"
        );
        assert!(xml.contains("<Type>6</Type>"), "Outbox must have Type=6");
        assert!(
            xml.contains("<Type>12</Type>"),
            "Junk Email must have Type=12"
        );
        // Ensure old incorrect values are NOT present
        assert!(
            !xml.contains("<Type>7</Type>"),
            "Type=7 is Tasks, not Junk Email"
        );
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
                (
                    "text-part".to_string(),
                    JmapBodyValue {
                        value: "Plain text body".to_string(),
                        is_encoding_problem: None,
                    },
                ),
                (
                    "html-part".to_string(),
                    JmapBodyValue {
                        value: "<p>HTML body</p>".to_string(),
                        is_encoding_problem: None,
                    },
                ),
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
            body_values: Some(HashMap::from([(
                "html-part".to_string(),
                JmapBodyValue {
                    value: "<p>HTML only</p>".to_string(),
                    is_encoding_problem: None,
                },
            )])),
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

    #[test]
    fn test_unescape_xml_text_basic_entities() {
        // &amp; → &, &lt; → <, &gt; → >, &quot; → ", &apos; → '
        assert_eq!(unescape_xml_text("&amp;"), "&");
        assert_eq!(unescape_xml_text("&lt;"), "<");
        assert_eq!(unescape_xml_text("&gt;"), ">");
        assert_eq!(unescape_xml_text("&quot;"), "\"");
        assert_eq!(unescape_xml_text("&apos;"), "'");
    }

    #[test]
    fn test_unescape_xml_text_multiple_entities() {
        assert_eq!(unescape_xml_text("Tom &amp; Jerry"), "Tom & Jerry");
        assert_eq!(unescape_xml_text("&lt;b&gt;bold&lt;/b&gt;"), "<b>bold</b>");
    }

    #[test]
    fn test_unescape_xml_text_no_entities() {
        assert_eq!(unescape_xml_text("Hello World"), "Hello World");
        assert_eq!(unescape_xml_text(""), "");
    }

    #[test]
    fn test_parse_ews_message_unescapes_xml_entities() {
        let xml = r#"
<t:Message>
 <t:Subject>Q&amp;A: &lt;Important&gt;</t:Subject>
 <t:Body BodyType="Text">A &amp; B &lt; C &gt; D</t:Body>
 <t:ToRecipients>
  <t:Mailbox><t:EmailAddress>user&amp;tag@example.com</t:EmailAddress></t:Mailbox>
 </t:ToRecipients>
</t:Message>"#;
        let msg = parse_ews_message(xml).expect("Should parse message");
        assert_eq!(msg.subject, "Q&A: <Important>");
        assert_eq!(msg.body, "A & B < C > D");
        assert_eq!(msg.to_recipients, vec!["user&tag@example.com"]);
    }

    #[test]
    fn test_eas_collection_id_to_mailbox_role_mapping() {
        assert_eq!(eas_collection_id_to_mailbox_role("2"), Some("inbox"));
        assert_eq!(eas_collection_id_to_mailbox_role("3"), Some("drafts"));
        assert_eq!(eas_collection_id_to_mailbox_role("4"), Some("trash"));
        assert_eq!(eas_collection_id_to_mailbox_role("5"), Some("sent"));
        assert_eq!(eas_collection_id_to_mailbox_role("6"), None); // Outbox
        assert_eq!(eas_collection_id_to_mailbox_role("12"), Some("junk"));
        assert_eq!(eas_collection_id_to_mailbox_role("7"), None); // Tasks, not Junk Email
    }

    #[test]
    fn test_eas_collection_id_to_mailbox_role_unknown_returns_none() {
        assert_eq!(eas_collection_id_to_mailbox_role("99"), None);
    }

    #[test]
    fn test_is_eas_email_collection_id() {
        // Email folders: 2=Inbox, 3=Drafts, 4=Deleted, 5=Sent, 6=Outbox, 12=Junk
        assert!(is_eas_email_collection_id("2"));
        assert!(is_eas_email_collection_id("3"));
        assert!(is_eas_email_collection_id("4"));
        assert!(is_eas_email_collection_id("5"));
        assert!(is_eas_email_collection_id("6"));
        assert!(is_eas_email_collection_id("12"));
        // Calendar: 1
        assert!(!is_eas_email_collection_id("1"));
        // Unknown
        assert!(!is_eas_email_collection_id("0"));
        assert!(!is_eas_email_collection_id("7"));
        assert!(!is_eas_email_collection_id("99"));
    }
}
