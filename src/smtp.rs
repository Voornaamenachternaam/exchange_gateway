// src/smtp.rs
//
// SMTP submission client for sending email through Stalwart Mailserver.
// Uses lettre with tokio1-rustls-tls for SMTPS on port 465.
//
// Per MS-OXSMTP and MS-OXWSCORE §3.1.4.8, when a client sends email via EWS
// (SendItem, CreateItem with MessageDisposition="SendOnly"/"SendAndSaveCopy")
// or EAS (SendMail, SmartReply, SmartForward), the gateway submits the
// message to the Stalwart SMTP server on behalf of the authenticated user.

use lettre::message::header::ContentType;
use lettre::message::{Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use secrecy::{ExposeSecret, SecretString};
use tracing::{debug, error, info, warn};

/// SMTP client for email submission.
///
/// Creates a new transport per request (lettre transports are not Clone).
/// The overhead is acceptable because SMTP connections are short-lived.
#[derive(Clone)]
pub struct SmtpClient {
    host: String,
    port: u16,
    /// Whether to use implicit TLS (SMTPS on port 465) vs STARTTLS (port 587)
    #[allow(dead_code)]
    implicit_tls: bool,
}

/// Result of an SMTP send operation.
#[derive(Debug)]
pub struct SendResult {
    pub message_id: String,
    pub submitted_at: chrono::DateTime<chrono::Utc>,
}

/// Parameters for sending an email message via SMTP.
pub struct SendEmailParams<'a> {
    pub from: &'a str,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: &'a str,
    pub text_body: &'a str,
    pub html_body: Option<&'a str>,
    pub username: &'a str,
    pub password: &'a SecretString,
}

impl SmtpClient {
    /// Create a new SMTP client configuration.
    ///
    /// `host` is the Stalwart SMTP hostname (e.g., "stalwart").
    /// `port` is the SMTP port (465 for SMTPS, 587 for STARTTLS).
    pub fn new(host: &str, port: u16) -> Self {
        let implicit_tls = port == 465;
        Self {
            host: host.to_string(),
            port,
            implicit_tls,
        }
    }

    /// Build an SMTP transport for the given credentials.
    ///
    /// For port 465, uses implicit TLS (SMTPS — `relay()`).
    /// For port 587, uses STARTTLS (`starttls_relay()`).
    fn build_transport(
        &self,
        username: &str,
        password: &SecretString,
    ) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        let creds = Credentials::new(username.to_string(), password.expose_secret().to_string());

        let transport = if self.implicit_tls {
            // Port 465: SMTPS (implicit TLS)
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)?
                .port(self.port)
                .credentials(creds)
                .build()
        } else {
            // Port 587: STARTTLS
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)?
                .port(self.port)
                .credentials(creds)
                .build()
        };
        Ok(transport)
    }

    /// Build a bare transport without credentials (for health checks).
    fn build_bare_transport(&self) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        let transport = if self.implicit_tls {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&self.host)?
                .port(self.port)
                .build()
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&self.host)?
                .port(self.port)
                .build()
        };
        Ok(transport)
    }

    /// Send an email message via SMTP.
    ///
    /// This method builds a MIME message from the provided fields and
    /// submits it to the Stalwart SMTP server with the user's credentials.
    ///
    /// Per MS-OXWSCORE §3.1.4.8, the server authenticates as the user
    /// who is sending the message. The SMTP envelope MAIL FROM is set
    /// to the sender's email address.
    pub async fn send_email(&self, params: SendEmailParams<'_>) -> anyhow::Result<SendResult> {
        let from_mailbox: Mailbox = params
            .from
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid from address '{}': {}", params.from, e))?;

        let mut builder = Message::builder()
            .from(from_mailbox)
            .subject(params.subject)
            .date_now();

        for recipient in &params.to {
            let mailbox: Mailbox = recipient
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid to address '{}': {}", recipient, e))?;
            builder = builder.to(mailbox);
        }
        for recipient in &params.cc {
            let mailbox: Mailbox = recipient
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid cc address '{}': {}", recipient, e))?;
            builder = builder.cc(mailbox);
        }
        for recipient in &params.bcc {
            let mailbox: Mailbox = recipient
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid bcc address '{}': {}", recipient, e))?;
            builder = builder.bcc(mailbox);
        }

        let message = if let Some(html) = params.html_body {
            // Multi-part: text + HTML
            builder.multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(params.text_body.to_string()),
                    )
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_HTML)
                            .body(html.to_string()),
                    ),
            )?
        } else {
            // Plain text only
            builder.body(params.text_body.to_string())?
        };

        let transport = self.build_transport(params.username, params.password)?;

        // Extract the lettre-generated Message-ID before sending (send takes ownership).
        // Per RFC 5322 §3.6.4, the Message-ID is the universal identifier for the
        // email, enabling correlation with the copy in the Sent Items folder.
        let message_id = message
            .headers()
            .get::<lettre::message::header::MessageId>()
            .map(|h| h.as_ref().to_string())
            .unwrap_or_else(|| {
                format!(
                    "{}-{}",
                    chrono::Utc::now().timestamp_millis(),
                    uuid::Uuid::new_v4().simple()
                )
            });

        debug!(
            target: "smtp",
            host = %self.host,
            port = self.port,
            from = %params.from,
            to_count = params.to.len(),
            subject_len = params.subject.len(),
            "Sending email via SMTP"
        );

        match transport.send(message).await {
            Ok(response) => {
                info!(
                    target: "smtp",
                    host = %self.host,
                    from = %params.from,
                    to_count = params.to.len(),
                    response_code = %response.code(),
                    "Email sent successfully via SMTP"
                );
                Ok(SendResult {
                    message_id,
                    submitted_at: chrono::Utc::now(),
                })
            }
            Err(e) => {
                error!(
                    target: "smtp",
                    host = %self.host,
                    from = %params.from,
                    to_count = params.to.len(),
                    error = %e,
                    "Failed to send email via SMTP"
                );
                Err(anyhow::anyhow!("SMTP send failed: {}", e))
            }
        }
    }

    /// Send an iMIP (RFC 6047) meeting response — a `text/calendar;
    /// method=REPLY` MIME part addressed to the meeting organizer.
    ///
    /// Used by C4 (EWS AcceptItem/DeclineItem/TentativelyAcceptItem and EAS
    /// meeting-response) to deliver the attendee's PARTSTAT back to the
    /// organizer via SMTP. The `ics` body is the full VCALENDAR with
    /// `METHOD:REPLY` produced by [`crate::meeting::MeetingMessageGenerator`].
    /// Optional `text_body` is sent as an alternative `text/plain` part so
    /// mail clients without calendar support still render a readable reply.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_imip(
        &self,
        from: &str,
        to: Vec<String>,
        subject: &str,
        ics: &str,
        text_body: Option<&str>,
        username: &str,
        password: &SecretString,
    ) -> anyhow::Result<SendResult> {
        if to.is_empty() {
            return Err(anyhow::anyhow!("iMIP reply has no recipients (organizer)"));
        }
        if from.is_empty() {
            return Err(anyhow::anyhow!("iMIP reply has no sender (responder)"));
        }
        let from_mailbox: Mailbox = from
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid from address '{}': {}", from, e))?;

        let mut builder = Message::builder()
            .from(from_mailbox)
            .subject(subject)
            .date_now()
            .header(lettre::message::header::ContentDisposition::inline())
            // RFC 6047 §3.2: iMIP messages MUST set MIME-Version and a
            // Content-Type of text/calendar; method=REPLY.
            .header(lettre::message::header::ContentType::parse(
                "text/calendar; method=REPLY; charset=utf-8",
            )
            .map_err(|e| anyhow::anyhow!("Invalid iMIP content-type: {}", e))?);

        for recipient in &to {
            let mailbox: Mailbox = recipient
                .parse()
                .map_err(|e| anyhow::anyhow!("Invalid recipient '{}': {}", recipient, e))?;
            builder = builder.to(mailbox);
        }

        let calendar_part = SinglePart::builder()
            .header(lettre::message::header::ContentType::parse(
                "text/calendar; method=REPLY; charset=utf-8",
            )
            .map_err(|e| anyhow::anyhow!("Invalid iMIP part content-type: {}", e))?)
            .header(lettre::message::header::ContentDisposition::inline())
            .body(ics.to_string());

        let message = if let Some(text) = text_body {
            // multipart/alternative with text/plain fallback — clients that do
            // not understand text/calendar render the human-readable reply.
            builder.multipart(
                MultiPart::alternative()
                    .singlepart(
                        SinglePart::builder()
                            .header(ContentType::TEXT_PLAIN)
                            .body(text.to_string()),
                    )
                    .singlepart(calendar_part),
            )?
        } else {
            builder.singlepart(calendar_part)?
        };

        let transport = self.build_transport(username, password)?;
        let message_id = message
            .headers()
            .get::<lettre::message::header::MessageId>()
            .map(|h| h.as_ref().to_string())
            .unwrap_or_else(|| {
                format!(
                    "{}-{}",
                    chrono::Utc::now().timestamp_millis(),
                    uuid::Uuid::new_v4().simple()
                )
            });
        match transport.send(message).await {
            Ok(response) => {
                info!(
                    target: "smtp",
                    host = %self.host,
                    from = %from,
                    recipient_count = to.len(),
                    response_code = %response.code(),
                    "iMIP reply sent via SMTP"
                );
                Ok(SendResult {
                    message_id,
                    submitted_at: chrono::Utc::now(),
                })
            }
            Err(e) => {
                error!(target: "smtp", host = %self.host, from = %from, error = %e, "Failed to send iMIP reply");
                Err(anyhow::anyhow!("SMTP iMIP send failed: {}", e))
            }
        }
    }

    /// Health check: verify SMTP server is reachable.
    ///
    /// Attempts a connection without sending mail.
    /// Returns Ok(()) if the server accepts connections, Err otherwise.
    pub async fn health_check(&self) -> anyhow::Result<()> {
        let transport = self.build_bare_transport()?;

        match transport.test_connection().await {
            Ok(true) => {
                debug!(target: "smtp", host = %self.host, port = self.port, "SMTP health check passed");
                Ok(())
            }
            Ok(false) => {
                warn!(target: "smtp", host = %self.host, port = self.port, "SMTP health check failed: connection rejected");
                Err(anyhow::anyhow!("SMTP server rejected connection"))
            }
            Err(e) => {
                warn!(target: "smtp", host = %self.host, port = self.port, error = %e, "SMTP health check failed");
                Err(anyhow::anyhow!("SMTP health check failed: {}", e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_client_new_465_implicit_tls() {
        let client = SmtpClient::new("stalwart", 465);
        assert!(client.implicit_tls);
        assert_eq!(client.port, 465);
    }

    #[test]
    fn test_smtp_client_new_587_starttls() {
        let client = SmtpClient::new("stalwart", 587);
        assert!(!client.implicit_tls);
        assert_eq!(client.port, 587);
    }

    #[test]
    fn test_smtp_client_default_port_is_465() {
        let client = SmtpClient::new("stalwart", 465);
        assert_eq!(client.port, 465);
        assert!(client.implicit_tls);
    }
}
//
