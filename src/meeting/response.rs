// src/meeting/response.rs
//
// Meeting-response pipeline for C4: handles EWS AcceptItem /
// DeclineItem / TentativelyAcceptItem and EAS meeting-response by parsing the
// incoming iCalendar METHOD:REQUEST, building an iTIP REPLY (RFC 5546 §3.2.3),
// delivering it back to the meeting organizer via SMTP (RFC 6047), and updating
// the local attendee's PARTSTAT on the calendar copy.
//
// Why SMTP (not JMAP) for the iMIP reply: Stalwart's JMAP EmailSubmission/set
// (RFC 8621 §2.7) as exposed by our JmapClient::submit_email only supports
// text/plain + text/html bodyValues — it cannot emit a `text/calendar;
// method=REPLY` MIME part. A scheduling reply MUST be delivered as a proper
// iMIP message, so the gateway sends it over SMTP (port 465/587) where lettre
// can build the exact MIME structure. This is the correct protocol boundary:
// calendar *state* still travels over JMAP Calendar / CalDAV; the iTIP
// *transport* message uses SMTP because that is the canonical iMIP channel.

use crate::calendar::parse_ics_event;
use crate::meeting::attendee::AttendeeStatus;
use crate::meeting::message::{MeetingMessage, MeetingMessageGenerator};
use crate::models::AppState;
use crate::util::user_primary_email;
use anyhow::Result;
use std::sync::Arc;

/// The attendee's accept/decline/tentative decision (C4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseDecision {
    Accept,
    Decline,
    Tentative,
}

impl ResponseDecision {
    pub fn as_attendee_status(self) -> AttendeeStatus {
        match self {
            Self::Accept => AttendeeStatus::Accepted,
            Self::Decline => AttendeeStatus::Declined,
            Self::Tentative => AttendeeStatus::Tentative,
        }
    }

    pub fn as_partstat(self) -> &'static str {
        match self {
            Self::Accept => "ACCEPTED",
            Self::Decline => "DECLINED",
            Self::Tentative => "TENTATIVE",
        }
    }

    /// One-line human-readable summary for the text/plain alt body of the reply.
    fn summary_line(self) -> &'static str {
        match self {
            Self::Accept => "Accepted",
            Self::Decline => "Declined",
            Self::Tentative => "Tentatively accepted",
        }
    }
}

/// Parsed meeting invitation extracted from a `METHOD:REQUEST` iCalendar.
#[derive(Clone, Debug)]
pub struct MeetingInvitation {
    pub uid: String,
    pub sequence: u32,
    pub organizer_email: String,
    pub organizer_name: Option<String>,
    pub subject: String,
    pub start: chrono::DateTime<chrono::Utc>,
    pub end: chrono::DateTime<chrono::Utc>,
}

/// Parse the raw iCalendar of a meeting request into a [`MeetingInvitation`].
///
/// Accepts either the full VCALENDAR (with METHOD:REQUEST) or just the VEVENT.
/// Returns `None` when the UID or organizer email cannot be determined.
pub fn parse_meeting_request(ics: &str) -> Option<MeetingInvitation> {
    let item = parse_ics_event(ics)?;
    let organizer_email = item.organizer_email.clone().filter(|e| !e.is_empty())?;
    Some(MeetingInvitation {
        uid: item.uid.clone(),
        sequence: parse_sequence_from_ics(ics),
        organizer_email,
        organizer_name: item.organizer_name.clone(),
        subject: item.subject.clone(),
        start: item.start,
        end: item.end,
    })
}

/// Parse the SEQUENCE property straight from the raw ICS text. Returns 0 when
/// absent (RFC 5545 §3.8.7.4 default).
fn parse_sequence_from_ics(ics: &str) -> u32 {
    for line in ics.lines() {
        let line = line.trim();
        // A SEQUENCE property line looks like `SEQUENCE:2` (possibly with
        // parameters: `SEQUENCE;X-FOO=bar:2`). Continuation lines (leading
        // whitespace) are skipped here; folded values are rare for SEQUENCE.
        if line.starts_with("SEQUENCE")
            && let Some((_, value)) = line.split_once(':')
        {
            return value.trim().parse().unwrap_or(0);
        }
    }
    0
}

/// Build the iTIP REPLY iCalendar (METHOD:REPLY) for the given decision.
pub fn build_reply_ics(
    inv: &MeetingInvitation,
    decision: ResponseDecision,
    responder_email: &str,
    responder_name: Option<&str>,
) -> String {
    let msg = MeetingMessage::new_response(&crate::meeting::message::ResponseParams {
        uid: &inv.uid,
        organizer_email: &inv.organizer_email,
        subject: &inv.subject,
        start: inv.start,
        end: inv.end,
        status: decision.as_attendee_status(),
        sequence: inv.sequence,
        responder_email,
        responder_name,
    });
    let generator = MeetingMessageGenerator::new();
    generator.generate_ical(&msg)
}

/// Build a human-readable text/plain alt body summarising the response.
pub fn build_reply_text(
    inv: &MeetingInvitation,
    decision: ResponseDecision,
    responder_name: Option<&str>,
) -> String {
    let who = responder_name.unwrap_or("");
    let verb = decision.summary_line();
    format!(
        "{who}{sep}has {verb} the meeting invitation \"{subject}\".\r\n\r\nStart: {start}\r\nEnd:   {end}\r\nOrganizer: {org}\r\n",
        sep = if who.is_empty() { "" } else { " " },
        subject = inv.subject,
        start = inv.start.format("%Y-%m-%d %H:%M:%SZ"),
        end = inv.end.format("%Y-%m-%d %H:%M:%SZ"),
        org = inv.organizer_email,
    )
}

/// Send an iTIP REPLY to the meeting organizer via SMTP and optionally save a
/// local copy. Mirrors [`crate::email::send_email`]'s preference order but
/// always falls through to SMTP because JMAP EmailSubmission cannot emit
/// `text/calendar` MIME parts (see module docs).
///
/// `owner_username` is the canonicalized account owner (local-part username),
/// used to derive the responder's primary SMTP and SMTP submission credentials.
/// Returns the SMTP message-id of the sent reply.
pub async fn submit_meeting_response(
    state: &Arc<AppState>,
    inv: &MeetingInvitation,
    decision: ResponseDecision,
    owner_username: &str,
    password: &secrecy::SecretString,
) -> Result<String> {
    let responder_email = user_primary_email(owner_username, &state.cfg.mail_domain)
        .unwrap_or_else(|| {
            // Last-resort: assume owner is already a full email address.
            owner_username.to_string()
        });

    let ics = build_reply_ics(inv, decision, &responder_email, None);
    let text = build_reply_text(inv, decision, None);
    let subject = format!(
        "{}: {}",
        match decision {
            ResponseDecision::Accept => "Accepted",
            ResponseDecision::Decline => "Declined",
            ResponseDecision::Tentative => "Tentative",
        },
        inv.subject
    );

    // Prefer SMTP for the iMIP reply (see module doc). JMAP cannot emit the
    // required text/calendar MIME part.
    if let Some(smtp) = state.smtp_client.as_ref() {
        let result = smtp
            .send_imip(&crate::smtp::SendImipParams {
                from: &responder_email,
                to: vec![inv.organizer_email.clone()],
                subject: &subject,
                ics: &ics,
                text_body: Some(&text),
                username: owner_username,
                password,
            })
            .await?;
        tracing::info!(
            target: "meeting",
            uid = %inv.uid,
            decision = ?decision,
            message_id = %result.message_id,
            "Sent iTIP REPLY to organizer via SMTP"
        );
        return Ok(result.message_id);
    }

    Err(anyhow::anyhow!(
        "SMTP client not configured; cannot deliver iTIP reply (JMAP EmailSubmission does not support text/calendar MIME)"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_request() -> String {
        [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "PRODID:-//Test//EN",
            "METHOD:REQUEST",
            "BEGIN:VEVENT",
            "UID:event-123@example.com",
            "SEQUENCE:2",
            "DTSTAMP:20260701T120000Z",
            "DTSTART:20260710T100000Z",
            "DTEND:20260710T110000Z",
            "SUMMARY:Quarterly review",
            "ORGANIZER;CN=Ada Lovelace:mailto:ada@example.com",
            "ATTENDEE;ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:bob@example.com",
            "END:VEVENT",
            "END:VCALENDAR",
            "",
        ]
        .join("\r\n")
    }

    #[test]
    fn parse_request_extracts_organizer_and_uid() {
        let inv = parse_meeting_request(&sample_request()).expect("parse");
        assert_eq!(inv.uid, "event-123@example.com");
        assert_eq!(inv.organizer_email, "ada@example.com");
        assert_eq!(inv.organizer_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(inv.subject, "Quarterly review");
    }

    #[test]
    fn parse_request_without_organizer_returns_none() {
        let bad = [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "METHOD:REQUEST",
            "BEGIN:VEVENT",
            "UID:x@example.com",
            "SUMMARY:n",
            "DTSTART:20260710T100000Z",
            "DTEND:20260710T110000Z",
            "END:VEVENT",
            "END:VCALENDAR",
            "",
        ]
        .join("\r\n");
        assert!(parse_meeting_request(&bad).is_none());
    }

    #[test]
    fn reply_ics_has_method_reply_and_attendee_is_responder() {
        let inv = parse_meeting_request(&sample_request()).unwrap();
        let ics = build_reply_ics(
            &inv,
            ResponseDecision::Accept,
            "bob@example.com",
            Some("Bob"),
        );
        assert!(ics.contains("METHOD:REPLY"), "missing METHOD:REPLY: {ics}");
        assert!(ics.contains("UID:event-123@example.com"), "{}", ics);
        // ORGANIZER points at the meeting organizer; ATTENDEE points at the responder.
        assert!(ics.contains("ORGANIZER"), "{ics}");
        assert!(ics.contains("mailto:bob@example.com"), "{ics}");
        assert!(ics.contains("PARTSTAT=ACCEPTED"), "{ics}");
        // Must NOT include the organizer as the attendee (the original bug).
        let responder_block = ics
            .split("ATTENDEE")
            .nth(1)
            .unwrap_or("")
            .split(char::is_whitespace)
            .next()
            .unwrap_or("");
        assert!(responder_block.contains("bob@example.com"), "{ics}");
    }

    #[test]
    fn reply_ics_decline_uses_declined_partstat() {
        let inv = parse_meeting_request(&sample_request()).unwrap();
        let ics = build_reply_ics(&inv, ResponseDecision::Decline, "bob@example.com", None);
        assert!(ics.contains("METHOD:REPLY"));
        assert!(ics.contains("PARTSTAT=DECLINED"), "{ics}");
    }

    #[test]
    fn reply_text_mentions_decision_and_subject() {
        let inv = parse_meeting_request(&sample_request()).unwrap();
        let txt = build_reply_text(&inv, ResponseDecision::Tentative, Some("Bob"));
        assert!(txt.contains("Tentatively"));
        assert!(txt.contains("Quarterly review"));
        assert!(txt.contains("Ada Lovelace") || txt.contains("ada@example.com"));
    }

    #[test]
    fn sequence_extractor_reads_numeric_sequence() {
        assert_eq!(parse_sequence_from_ics(&sample_request()), 2);
        let no_seq = ["BEGIN:VEVENT", "UID:x@y", "END:VEVENT", ""].join("\r\n");
        assert_eq!(parse_sequence_from_ics(&no_seq), 0);
    }
}
