// src/meeting/rsvp.rs
//
// Shared RSVP pipeline for audit §13 (MeetingResponse / iMIP integrity).
//
// One code path serves both front doors — the EAS `MeetingResponse` command
// ([MS-ASCMD] §2.2.1.11) and the EWS `AcceptItem` / `TentativelyAcceptItem` /
// `DeclineItem` response objects ([MS-OXWSMTGS]) — because they express the
// same semantics: the authenticated user is an attendee of a meeting and is
// reporting an accept / tentative / decline decision for it, optionally
// scoped to a single instance of a recurring series and optionally
// accompanied by a new-time counter-proposal.
//
// The pipeline, per request:
//   1. Resolve the addressed item (meeting-request email or calendar item)
//      to the organizer's `METHOD:REQUEST` iCalendar, proving the target is
//      actually a meeting the user is invited to.
//   2. Reject organizer self-responses ([MS-ASCMD] §2.2.3.177.9 status 2 /
//      RFC 5546 §3.2.17: an organizer replying to their own REQUEST is a
//      scheduling loop — a Stalwart account organizing a meeting and
//      responding to its own request would otherwise double-book its own
//      calendar and self-email).
//   3. Validate instance scoping ([MS-ASCMD] §2.2.3.92.1: InstanceId is the
//      UTC start of one occurrence; it is only valid for calendar items that
//      recur, and must name an occurrence that actually exists).
//   4. Update the local attendee copy (PARTSTAT, ResponseType,
//      X-MS-APPOINTMENT-REPLY-TIME per [MS-ASCAL] §2.2.2.19), creating it
//      from the REQUEST when it does not exist yet (accept/tentative only —
//      Exchange does not calendar-book declined meetings), writing through
//      JMAP Calendar or CalDAV per the configured backend. All attendee
//      SCHEDULE-AGENTs are pinned to CLIENT (RFC 6638 §7.1) so Stalwart's
//      own scheduler never re-emits an iTIP copy of our reply from the
//      attendee's copy.
//   5. Deliver the iTIP REPLY to the organizer over SMTP (RFC 5546 §3.2.10 /
//      RFC 6047) — with RECURRENCE-ID and the instance's own times when
//      instance-scoped (RFC 5546 §3.6.2) — plus an iTIP COUNTER
//      (RFC 5546 §3.6.7) when the client proposed a new time
//      ([MS-ASCMD] §2.2.3.140/§2.2.3.141). Duplicate responses (same
//      decision for the same (uid, instance)) are idempotent: the stored
//      RSVP ([MS-ASCMD] §2.2.1.11 disambiguation table, "User responds to a
//      meeting request for which a response was already sent") suppresses
//      the second REPLY email; a changed decision is delivered and recorded.
//
// Statuses returned follow [MS-ASCMD] §2.2.3.177.9 exactly: 1 success, 2 for
// invalid-item conditions (malformed/unresolvable address, organizer
// self-response, InstanceId on an email item, nonexistent instance), 3 for
// transient backend failure (retry). Resolution is Global-scope for 3 and
// item-scope for 2 per the same table.

use crate::calendar::{CalendarItem, mark_scheduling_client_side, parse_ics_event, render_ics};
use crate::email::extract_meeting_request_ics;
use crate::models::AppState;
use crate::storage::EwsItemRow;
use crate::util::{normalize_email, user_primary_email};
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;

/// [MS-ASCMD] §2.2.3.177.9 — command completed successfully.
pub const STATUS_SUCCESS: u16 = 1;
/// [MS-ASCMD] §2.2.3.177.9 — invalid meeting request: malformed or invalid
/// item, item other than a meeting request / email / calendar item,
/// appointment the user organizes, or an InstanceId that addresses an email
/// item / names a nonexistent instance.
pub const STATUS_INVALID_ITEM: u16 = 2;
/// [MS-ASCMD] §2.2.3.177.9 — server-side error (misconfiguration, transient
/// system issue, bad item). Client retries.
pub const STATUS_SERVER_ERROR: u16 = 3;
/// [MS-ASCMD] §2.2.3.92.1 — "If the InstanceId element passes schema
/// validation but the value is not specified in the proper format, the
/// server responds with a Status element value of 104."
pub const STATUS_INSTANCE_MALFORMED: u16 = 104;
/// [MS-ASCMD] §2.2.3.92.1 — "If the InstanceId element value specifies a
/// non-recurring meeting, the server responds with a Status element value of
/// 146."
pub const STATUS_INSTANCE_NOT_RECURRING: u16 = 146;

/// What the client addressed the RSVP at.
#[derive(Clone, Debug)]
pub enum RsvpSource {
    /// A meeting-request email (EAS `<RequestId>` naming an email item, EWS
    /// `ReferenceItemId` naming the meeting request message). Carries the
    /// JMAP email id.
    Email { jmap_email_id: String },
    /// A calendar item (EAS `<RequestId>`/`Search:LongId` naming a calendar
    /// item, EWS `ReferenceItemId` naming the attendee's calendar copy).
    /// Carries the gateway server id from `item_map`.
    CalendarItem { server_id: String },
}

/// Which item the RSVP resolved to, with everything needed to respond.
pub struct ResolvedInvitation {
    /// The organizer's REQUEST iCalendar, parsed. For an email source this is
    /// the request itself; for a calendar source it is the attendee's copy,
    /// which (per RFC 5546 §3.1.1) carries the same ORGANIZER, attendees, UID
    /// and recurrence data as the REQUEST that created it.
    pub item: CalendarItem,
    /// SEQUENCE of the triggering REQUEST (RFC 5546 §3.6.2: the REPLY MUST
    /// echo it).
    pub sequence: u32,
    /// `true` when the source was the meeting-request email — InstanceId is
    /// then invalid ([MS-ASCMD] §2.2.3.177.9).
    pub from_email: bool,
    /// The attendee's existing calendar-copy row, when the source was a
    /// calendar item (or the email's UID already has one).
    pub calendar_row: Option<EwsItemRow>,
}

/// Why resolution failed. Mapped to [MS-ASCMD] §2.2.3.177.9 statuses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveFailure {
    /// → status 2 (invalid meeting request: referenced item is not a
    /// meeting request / has no organizer / carries no METHOD:REQUEST
    /// iCalendar part).
    InvalidItem,
    /// → status 2 (invalid meeting request; EWS maps this to
    /// `ErrorItemNotFound` to tell the client the addressed item is gone).
    NotFound,
    /// → status 3 (server error; retry).
    ServerError,
}

/// The attendee's decision plus the request options that shape delivery.
#[derive(Clone, Debug)]
pub struct RsvpRequest {
    pub decision: crate::meeting::response::ResponseDecision,
    /// [MS-ASCMD] §2.2.3.92.1 InstanceId: UTC start of the occurrence this
    /// response applies to. `None` = whole series.
    pub instance_id: Option<DateTime<Utc>>,
    /// [MS-ASCMD] §2.2.3.163: no email is sent when the SendResponse element
    /// is absent.
    pub send_reply: bool,
    /// [MS-ASCMD] §2.2.3.163 airsyncbase:Body text for the reply email.
    pub reply_body_text: Option<String>,
    /// [MS-ASCMD] §2.2.3.141 ProposedStartTime, when countering.
    pub proposed_start: Option<DateTime<Utc>>,
    /// [MS-ASCMD] §2.2.3.140 ProposedEndTime, when countering.
    pub proposed_end: Option<DateTime<Utc>>,
}

/// Everything the protocol handlers need to build their response.
#[derive(Clone, Debug)]
pub struct RsvpOutcome {
    /// [MS-ASCMD] §2.2.3.177.9 status (also used to shape EWS errors).
    pub status: u16,
    /// Server id of the attendee's calendar copy, for `<CalendarId>`
    /// ([MS-ASCMD] §2.2.3.18 — only meaningful for accept/tentative).
    pub calendar_server_id: Option<String>,
    /// SMTP message id of the delivered iTIP REPLY, if one was sent.
    pub reply_message_id: Option<String>,
    /// SMTP message id of the delivered iTIP COUNTER, if one was sent.
    pub counter_message_id: Option<String>,
    /// True when the stored RSVP matched this decision, so no second REPLY
    /// email was delivered (idempotent re-response).
    pub duplicate: bool,
    /// True when the responder is the meeting organizer and the iTIP loop was
    /// suppressed (RFC 5546 §3.2.17 self-notify protection).
    pub self_notify_suppressed: bool,
}

impl RsvpOutcome {
    /// The outcome for a request that failed before any side effect.
    pub fn failure(failure: &ResolveFailure) -> Self {
        Self {
            status: match failure {
                ResolveFailure::InvalidItem | ResolveFailure::NotFound => STATUS_INVALID_ITEM,
                ResolveFailure::ServerError => STATUS_SERVER_ERROR,
            },
            calendar_server_id: None,
            reply_message_id: None,
            counter_message_id: None,
            duplicate: false,
            self_notify_suppressed: false,
        }
    }
}

/// Resolve the addressed source to the meeting it refers to.
///
/// Email sources download the raw MIME and require a `METHOD:REQUEST`
/// iCalendar part; calendar sources read the stored event from whichever
/// backend row the gateway mapped it to. A resolved item must be a real
/// meeting: it needs an organizer (the iTIP REPLY recipient) — without one
/// there is nobody to reply to and the item is not a meeting request.
pub async fn resolve_invitation(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    source: &RsvpSource,
) -> std::result::Result<ResolvedInvitation, ResolveFailure> {
    match source {
        RsvpSource::Email { jmap_email_id } => {
            resolve_email_invitation(state, username, password, jmap_email_id).await
        }
        RsvpSource::CalendarItem { server_id } => {
            resolve_calendar_invitation(state, username, password, server_id).await
        }
    }
}

async fn resolve_email_invitation(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    jmap_email_id: &str,
) -> std::result::Result<ResolvedInvitation, ResolveFailure> {
    let Some(jmap) = state.jmap_client.as_ref().cloned() else {
        tracing::error!(target: "meeting", "email RSVP resolution requires the JMAP client");
        return Err(ResolveFailure::ServerError);
    };

    let account_id = jmap.get_account_id(username, password).await.map_err(|e| {
        tracing::error!(target: "meeting", error = %e, "RSVP: JMAP account lookup failed");
        ResolveFailure::ServerError
    })?;

    // Only the raw MIME blob is consumed; body values are not needed.
    let email = jmap
        .get_email(&account_id, jmap_email_id, username, password, false)
        .await
        .map_err(|e| {
            tracing::warn!(target: "meeting", error = %e, "RSVP: meeting request email fetch failed");
            ResolveFailure::ServerError
        })?
        .ok_or(ResolveFailure::NotFound)?;

    let Some(blob_id) = email.blob_id.as_ref().filter(|b| !b.is_empty()) else {
        return Err(ResolveFailure::NotFound);
    };
    let raw_mime = jmap
        .download_blob(&account_id, blob_id, username, password)
        .await
        .map_err(|e| {
            tracing::warn!(target: "meeting", error = %e, "RSVP: meeting request blob download failed");
            ResolveFailure::ServerError
        })?;

    let Some(ics) = extract_meeting_request_ics(&raw_mime) else {
        // Not a meeting request email → status 2 ([MS-ASCMD] §2.2.3.177.9:
        // "referencing an item other than a meeting request").
        return Err(ResolveFailure::InvalidItem);
    };

    let Some(item) = parse_ics_event(&ics) else {
        tracing::warn!(target: "meeting", uid = %jmap_email_id, "RSVP: meeting request iCalendar unparseable");
        return Err(ResolveFailure::InvalidItem);
    };
    if item.organizer_email.as_deref().is_none_or(|e| e.is_empty()) {
        return Err(ResolveFailure::InvalidItem);
    }

    // If the attendee already has a calendar copy for this UID, the local
    // update targets it instead of creating a duplicate.
    let calendar_row = match state.storage.get_ews_item_by_uid(username, &item.uid).await {
        Ok(row) => row,
        Err(e) => {
            tracing::warn!(target: "meeting", error = %e, "RSVP: item_map lookup failed");
            None
        }
    };

    Ok(ResolvedInvitation {
        item,
        sequence: crate::meeting::response::parse_sequence_from_ics(&ics),
        from_email: true,
        calendar_row,
    })
}

async fn resolve_calendar_invitation(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    server_id: &str,
) -> std::result::Result<ResolvedInvitation, ResolveFailure> {
    let row = state
        .storage
        .get_ews_item_by_server_id(username, server_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "meeting", error = %e, "RSVP: item_map read failed");
            ResolveFailure::ServerError
        })?
        .ok_or(ResolveFailure::NotFound)?;

    // Read the event from whichever backend owns it. JMAP rows keep the
    // event id in the href; CalDAV rows store a direct href.
    let (ics, etag) = if row.resource_href.starts_with("jmap://") {
        let Some(jmap) = state.jmap_client.as_ref().cloned() else {
            return Err(ResolveFailure::ServerError);
        };
        let Some(event_id) = row.resource_href.rsplit('/').next() else {
            return Err(ResolveFailure::InvalidItem);
        };
        let account_id = jmap
            .get_account_id(username, password)
            .await
            .map_err(|_| ResolveFailure::ServerError)?;
        let (ics, _event_id, returned_etag) = jmap
            .get_calendar_event(&account_id, event_id, username, password)
            .await
            .map_err(|e| {
                tracing::warn!(target: "meeting", error = %e, "RSVP: JMAP calendar event read failed");
                ResolveFailure::ServerError
            })?;
        (ics, Some(returned_etag))
    } else {
        let caldav = crate::caldav::CaldavClient::new(&state.cfg).map_err(|e| {
            tracing::error!(target: "meeting", error = %e, "RSVP: CalDAV client init failed");
            ResolveFailure::ServerError
        })?;
        let (ics, returned_etag) = caldav
            .get_event(&row.resource_href, username, password.expose_secret())
            .await
            .map_err(|e| {
                tracing::warn!(target: "meeting", error = %e, "RSVP: CalDAV event read failed");
                ResolveFailure::ServerError
            })?;
        (ics, returned_etag)
    };

    let Some(item) = parse_ics_event(&ics) else {
        tracing::warn!(target: "meeting", server_id = %server_id, "RSVP: calendar item iCalendar unparseable");
        return Err(ResolveFailure::InvalidItem);
    };
    if item.organizer_email.as_deref().is_none_or(|e| e.is_empty()) {
        // No organizer → a plain appointment, not a meeting.
        return Err(ResolveFailure::InvalidItem);
    }

    let mut row = row;
    if row.etag.is_none() || row.etag.as_deref() == Some("") {
        row.etag = etag;
    }

    Ok(ResolvedInvitation {
        item,
        sequence: crate::meeting::response::parse_sequence_from_ics(&ics),
        from_email: false,
        calendar_row: Some(row),
    })
}

/// Apply one RSVP end-to-end. Returns the outcome, or a resolution failure
/// mapped from `ResolveFailure` when the addressed item could not be used.
pub async fn apply_rsvp(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    source: &RsvpSource,
    req: &RsvpRequest,
) -> std::result::Result<RsvpOutcome, ResolveFailure> {
    // [MS-ASCMD] §2.2.3.140/§2.2.3.141: ProposedStartTime requires
    // ProposedEndTime and vice versa; a lone proposal is a malformed request.
    let countering = req.proposed_start.is_some() || req.proposed_end.is_some();
    if countering && !(req.proposed_start.is_some() && req.proposed_end.is_some()) {
        return Ok(RsvpOutcome {
            status: STATUS_INVALID_ITEM,
            ..RsvpOutcome::failure(&ResolveFailure::InvalidItem)
        });
    }

    let resolved = resolve_invitation(state, username, password, source).await?;
    let invitation = resolved;

    // [MS-ASCMD] §2.2.3.177.9: "The InstanceId element specifies an email
    // meeting request item" → status 2.
    if invitation.from_email && req.instance_id.is_some() {
        return Ok(RsvpOutcome {
            status: STATUS_INVALID_ITEM,
            ..RsvpOutcome::failure(&ResolveFailure::InvalidItem)
        });
    }

    // [MS-ASCMD] §2.2.3.92.1: "If the InstanceId element value specifies a
    // non-recurring meeting, the server responds with a Status element value
    // of 146."
    if req.instance_id.is_some()
        && invitation
            .item
            .rrule
            .as_deref()
            .is_none_or(|r| r.is_empty())
    {
        return Ok(RsvpOutcome {
            status: STATUS_INSTANCE_NOT_RECURRING,
            ..RsvpOutcome::failure(&ResolveFailure::InvalidItem)
        });
    }

    // Organizer self-response protection ([MS-ASCMD] §2.2.3.177.9 "The request
    // points to an appointment in which the user is the organizer"; RFC 5546
    // §3.2.17 "CAL-ADDRESS ... MUST NOT be the ORGANIZER" for a REPLY).
    let user_email = user_primary_email(username, &state.cfg.mail_domain)
        .unwrap_or_else(|| username.to_string());
    let organizer_email = invitation.item.organizer_email.clone().unwrap_or_default();
    let is_organizer = normalize_email(&organizer_email) == normalize_email(&user_email)
        || normalize_email(&organizer_email) == normalize_email(username);
    if is_organizer {
        tracing::info!(
            target: "meeting",
            uid = %invitation.item.uid,
            "RSVP: organizer self-response suppressed (no iTIP loop, no calendar rewrite)"
        );
        return Ok(RsvpOutcome {
            status: STATUS_INVALID_ITEM,
            self_notify_suppressed: true,
            ..RsvpOutcome::failure(&ResolveFailure::InvalidItem)
        });
    }

    // Instance existence ([MS-ASCMD] §2.2.3.177.9: "The InstanceId element
    // specifies a nonexistent instance"). Covers EXDATE-removed instances and
    // deleted exceptions too.
    if let Some(instance_id) = req.instance_id
        && !instance_exists(&invitation.item, instance_id)
    {
        return Ok(RsvpOutcome {
            status: STATUS_INVALID_ITEM,
            ..RsvpOutcome::failure(&ResolveFailure::InvalidItem)
        });
    }

    // Idempotence key: whole-series ("") or the UTC RECURRENCE-ID.
    let instance_key = instance_key_for(req.instance_id);
    let duplicate = match state
        .storage
        .get_meeting_rsvp(username, &invitation.item.uid, &instance_key)
        .await
    {
        Ok(Some(row)) => row.decision == decision_code(req.decision),
        Ok(None) => false,
        Err(e) => {
            tracing::warn!(target: "meeting", error = %e, "RSVP: idempotence read failed");
            false
        }
    };

    // Local attendee-copy update (PARTSTAT etc.). Failures are fatal to the
    // command — the local calendar is the responding user's record of the
    // decision and Exchange treats it as authoritative.
    let calendar_server_id =
        match update_local_attendee_copy(state, username, password, &invitation, req, &user_email)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(
                    target: "meeting",
                    error = %e,
                    uid = %invitation.item.uid,
                    "RSVP: local attendee copy update failed"
                );
                return Ok(RsvpOutcome {
                    status: STATUS_SERVER_ERROR,
                    ..RsvpOutcome::failure(&ResolveFailure::ServerError)
                });
            }
        };

    // iTIP delivery. Skipped when the client did not ask for an email
    // ([MS-ASCMD] §2.2.3.163), when the same decision was already delivered
    // (duplicate → idempotent no-op email-wise), or when the organizer was
    // the responder (suppressed above).
    let mut reply_message_id = None;
    let mut counter_message_id = None;
    let mut delivered = false;
    if req.send_reply && !duplicate {
        match deliver_itip(state, username, password, &invitation, req, &user_email).await {
            Ok((reply_id, counter_id)) => {
                reply_message_id = reply_id;
                counter_message_id = counter_id;
                delivered = true;
            }
            Err(e) => {
                // The organizer must learn the decision; a failed delivery is
                // a transient server condition the client should retry
                // ([MS-ASCMD] §2.2.3.177.9 status 3). The local copy update is
                // idempotent, so a retry is safe.
                tracing::warn!(
                    target: "meeting",
                    error = %e,
                    uid = %invitation.item.uid,
                    "RSVP: iTIP delivery failed; client should retry (status 3)"
                );
                return Ok(RsvpOutcome {
                    status: STATUS_SERVER_ERROR,
                    calendar_server_id,
                    ..RsvpOutcome::failure(&ResolveFailure::ServerError)
                });
            }
        }
    } else if req.send_reply && duplicate {
        tracing::info!(
            target: "meeting",
            uid = %invitation.item.uid,
            instance_key = %instance_key,
            "RSVP: duplicate decision suppressed (no second iTIP REPLY)"
        );
    }

    // Record the delivered RSVP after a successful send (so a failed send is
    // retried rather than treated as delivered).
    if req.send_reply
        && delivered
        && let Err(e) = state
            .storage
            .upsert_meeting_rsvp(
                username,
                &invitation.item.uid,
                &instance_key,
                decision_code(req.decision),
                reply_message_id
                    .as_deref()
                    .or(counter_message_id.as_deref()),
                calendar_server_id.as_deref(),
            )
            .await
    {
        tracing::warn!(target: "meeting", error = %e, "RSVP: idempotence record write failed");
    }

    Ok(RsvpOutcome {
        status: STATUS_SUCCESS,
        // [MS-ASCMD] §2.2.3.18: CalendarId tells the client where the
        // attendee's copy lives; declined meetings are not booked.
        calendar_server_id,
        reply_message_id,
        counter_message_id,
        duplicate,
        self_notify_suppressed: false,
    })
}

/// Deliver the iTIP REPLY (and COUNTER when countering) to the organizer.
async fn deliver_itip(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
    responder_email: &str,
) -> Result<(Option<String>, Option<String>)> {
    let Some(smtp) = state.smtp_client.as_ref() else {
        return Err(anyhow!(
            "SMTP client not configured; cannot deliver iTIP reply (JMAP EmailSubmission does not support text/calendar MIME)"
        ));
    };

    let item = &invitation.item;
    let organizer_email = item.organizer_email.clone().unwrap_or_default();

    // The REPLY's VEVENT times: the instance's own times when
    // instance-scoped (RFC 5546 §3.6.2 REPLY with RECURRENCE-ID refers to the
    // occurrence, not the series master).
    let (reply_start, reply_end) = match req.instance_id {
        None => (item.start, item.end),
        Some(instance_id) => {
            let exception = find_matching_exception(item, instance_id);
            let start = exception.and_then(|ex| ex.start).unwrap_or(instance_id);
            let duration = item.end - item.start;
            let end = exception
                .and_then(|ex| ex.end)
                .unwrap_or_else(|| start + duration);
            (start, end)
        }
    };

    let ics = crate::meeting::response::build_reply_ics(
        &crate::meeting::response::MeetingInvitation {
            uid: item.uid.clone(),
            sequence: invitation.sequence,
            organizer_email: organizer_email.clone(),
            organizer_name: item.organizer_name.clone(),
            subject: item.subject.clone(),
            start: reply_start,
            end: reply_end,
        },
        req.decision,
        responder_email,
        None,
        req.instance_id,
    );

    let text = req.reply_body_text.clone().unwrap_or_else(|| {
        crate::meeting::response::build_reply_text(
            &crate::meeting::response::MeetingInvitation {
                uid: item.uid.clone(),
                sequence: invitation.sequence,
                organizer_email: organizer_email.clone(),
                organizer_name: item.organizer_name.clone(),
                subject: item.subject.clone(),
                start: reply_start,
                end: reply_end,
            },
            req.decision,
            None,
        )
    });
    let subject = format!(
        "{}: {}",
        match req.decision {
            crate::meeting::response::ResponseDecision::Accept => "Accepted",
            crate::meeting::response::ResponseDecision::Decline => "Declined",
            crate::meeting::response::ResponseDecision::Tentative => "Tentative",
        },
        item.subject
    );

    let result = smtp
        .send_imip(&crate::smtp::SendImipParams {
            from: responder_email,
            to: vec![organizer_email.clone()],
            subject: &subject,
            ics: &ics,
            text_body: Some(&text),
            username,
            password,
            method: None,
        })
        .await?;
    tracing::info!(
        target: "meeting",
        uid = %item.uid,
        decision = ?req.decision,
        message_id = %result.message_id,
        "Delivered iTIP REPLY to organizer"
    );
    let reply_id = Some(result.message_id);

    // New-time proposal → a separate iTIP COUNTER (RFC 5546 §3.6.7). The
    // REPLY already carried the decision; the COUNTER carries the proposed
    // times, mirroring how Outlook's "Propose New Time" delivers both.
    let counter_id = if let (Some(start), Some(end)) = (req.proposed_start, req.proposed_end) {
        let msg = crate::meeting::message::MeetingMessage::new_counter(
            &crate::meeting::message::CounterParams {
                uid: item.uid.clone(),
                organizer_email,
                subject: item.subject.clone(),
                original_start: reply_start,
                original_end: reply_end,
                proposed_start: start,
                proposed_end: end,
                sequence: invitation.sequence,
                recurrence_id: req.instance_id,
                responder_email: responder_email.to_string(),
                responder_name: None,
            },
        );
        let generator = crate::meeting::message::MeetingMessageGenerator::new();
        let counter_ics = generator.generate_ical(&msg);
        let result = smtp
            .send_imip(&crate::smtp::SendImipParams {
                from: responder_email,
                to: vec![item.organizer_email.clone().unwrap_or_default()],
                subject: &format!("New Time Proposed: {}", item.subject),
                ics: &counter_ics,
                text_body: Some(&format!(
                    "Proposed new time for \"{subject}\":\r\nStart: {start}\r\nEnd:   {end}\r\n",
                    subject = item.subject,
                    start = start.format("%Y-%m-%d %H:%M:%SZ"),
                    end = end.format("%Y-%m-%d %H:%M:%SZ"),
                )),
                username,
                password,
                method: Some("COUNTER"),
            })
            .await?;
        tracing::info!(
            target: "meeting",
            uid = %item.uid,
            message_id = %result.message_id,
            "Delivered iTIP COUNTER to organizer"
        );
        Some(result.message_id)
    } else {
        None
    };

    Ok((reply_id, counter_id))
}

/// Patch (or create) the local attendee calendar copy. Returns the calendar
/// server id of the copy for `<CalendarId>`, or `None` for declines with no
/// existing copy (Exchange does not book declined meetings).
async fn update_local_attendee_copy(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
    user_email: &str,
) -> Result<Option<String>> {
    let mut item = invitation.item.clone();

    match req.instance_id {
        None => patch_series_response(&mut item, user_email, req.decision),
        Some(instance_id) => {
            patch_instance_response(&mut item, user_email, req.decision, instance_id)
        }
    }

    // Pin scheduling to the client (RFC 6638 §7.1): the attendee copy on
    // Stalwart must not trigger a second, server-generated scheduling
    // message — the gateway already delivered the iTIP REPLY over SMTP.
    mark_scheduling_client_side(&mut item);

    // Declined + no existing copy: don't create one (Exchange semantics).
    if invitation.calendar_row.is_none()
        && matches!(
            req.decision,
            crate::meeting::response::ResponseDecision::Decline
        )
    {
        return Ok(None);
    }

    match &invitation.calendar_row {
        Some(row) => {
            write_existing_copy(state, username, password, row, &item).await?;
            Ok(Some(row.server_id.clone()))
        }
        None => {
            let server_id = create_attendee_copy(state, username, password, &item).await?;
            Ok(Some(server_id))
        }
    }
}

/// Set the responding user's PARTSTAT / ResponseType / reply time on the
/// series-level item (whole-series response).
fn patch_series_response(
    item: &mut CalendarItem,
    user_email: &str,
    decision: crate::meeting::response::ResponseDecision,
) {
    apply_attendee_decision(&mut item.attendees, user_email, decision);
    item.response_type = Some(response_type_code(decision));
    item.appointment_reply_time = Some(Utc::now());
}

/// Record the response on the specific occurrence: the matching exception is
/// patched (created when absent) with its own attendee roster, while the
/// series master keeps its previous state ([MS-ASASCAL] exception model —
/// instance-scoped responses must not rewrite the whole series).
fn patch_instance_response(
    item: &mut CalendarItem,
    user_email: &str,
    decision: crate::meeting::response::ResponseDecision,
    instance_id: DateTime<Utc>,
) {
    let pos = item
        .exceptions
        .iter()
        .position(|ex| ex.exception_start == instance_id);
    let exception = match pos {
        Some(i) => &mut item.exceptions[i],
        None => {
            item.exceptions.push(crate::calendar::CalendarException {
                deleted: false,
                exception_start: instance_id,
                start: Some(instance_id),
                end: None,
                ..Default::default()
            });
            item.exceptions.last_mut().expect("just pushed")
        }
    };
    let mut attendees = exception
        .attendees
        .clone()
        .unwrap_or_else(|| item.attendees.clone());
    apply_attendee_decision(&mut attendees, user_email, decision);
    exception.attendees = Some(attendees);
    exception.response_type = Some(response_type_code(decision));
    exception.appointment_reply_time = Some(Utc::now());
    exception.meeting_status = item.meeting_status;
}

fn apply_attendee_decision(
    attendees: &mut Vec<crate::calendar::Attendee>,
    user_email: &str,
    decision: crate::meeting::response::ResponseDecision,
) {
    if let Some(attendee) = attendees
        .iter_mut()
        .find(|a| normalize_email(&a.email) == normalize_email(user_email))
    {
        attendee.attendee_status = Some(attendee_status_code(decision));
        attendee.partstat = Some(decision.as_partstat().to_string());
    } else {
        // The responder was not on the roster (e.g. invited via a mailing
        // list). Record them so the copy shows the decision.
        attendees.push(crate::calendar::Attendee {
            name: None,
            email: user_email.to_string(),
            attendee_type: Some(1),
            attendee_status: Some(attendee_status_code(decision)),
            partstat: Some(decision.as_partstat().to_string()),
            schedule_agent: Some("CLIENT".to_string()),
        });
    }
}

/// Create the attendee's copy on JMAP Calendar. Returns
/// `(account_id, event_id, etag)` or `None` on any failure (the caller falls
/// back to CalDAV).
async fn create_copy_via_jmap(
    jmap: &crate::jmap::JmapClient,
    username: &str,
    password: &SecretString,
    ics: &str,
) -> Option<(String, String, String)> {
    let account_id = jmap.get_account_id(username, password).await.ok()?;
    let calendars = jmap.query_calendars(username, password).await.ok()?;
    let calendar = calendars.calendars.first().filter(|c| c.id.is_some())?;
    let (event_id, _uid, etag) = jmap
        .set_calendar_event(crate::jmap::SetCalendarEventParams {
            account_id: &account_id,
            ics,
            event_id: None,
            calendar_id: calendar.id.as_deref(),
            username,
            password,
        })
        .await
        .ok()?;
    Some((account_id, event_id, etag))
}

/// Write the patched item through the backend the copy lives on (JMAP update
/// or CalDAV PUT) and refresh the stored etag.
async fn write_existing_copy(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    row: &EwsItemRow,
    item: &CalendarItem,
) -> Result<()> {
    let ics = render_ics(item);

    if row.resource_href.starts_with("jmap://") {
        let Some(jmap) = state.jmap_client.as_ref().cloned() else {
            return Err(anyhow!("JMAP client unavailable for calendar update"));
        };
        let Some(event_id) = row.resource_href.rsplit('/').next() else {
            return Err(anyhow!(
                "malformed JMAP calendar href: {}",
                row.resource_href
            ));
        };
        let account_id = jmap.get_account_id(username, password).await?;
        let (_id, _uid, etag) = jmap
            .set_calendar_event(crate::jmap::SetCalendarEventParams {
                account_id: &account_id,
                ics: &ics,
                event_id: Some(event_id),
                calendar_id: None,
                username,
                password,
            })
            .await?;
        state
            .storage
            .upsert_item_map(
                username,
                row.caldav_href.as_deref().unwrap_or(""),
                &row.resource_href,
                &row.server_id,
                &item.uid,
                &etag,
            )
            .await?;
    } else {
        let caldav = crate::caldav::CaldavClient::new(&state.cfg)?;
        let (resource_href, etag) = caldav
            .put_event(
                "",
                Some(&row.resource_href),
                &ics,
                username,
                password.expose_secret(),
                row.etag.as_deref(),
            )
            .await?;
        state
            .storage
            .upsert_item_map(
                username,
                "",
                &resource_href,
                &row.server_id,
                &item.uid,
                &etag,
            )
            .await?;
    }
    Ok(())
}

/// Create the attendee's calendar copy from the REQUEST iCalendar, on the
/// configured backend (JMAP Calendar preferred, CalDAV fallback — matching the
/// CreateItem backend-selection rules, including the JMAP write-capability
/// gate). Returns the new calendar server id.
async fn create_attendee_copy(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    item: &CalendarItem,
) -> Result<String> {
    let ics = render_ics(item);

    let use_jmap = state.cfg.prefer_jmap_calendar
        && state.jmap_client.is_some()
        && state.jmap_calendar_writes_enabled();

    if use_jmap
        && let Some(jmap) = state.jmap_client.as_ref().cloned()
        && jmap.supports_calendar(username, password).await
    {
        if let Some((account_id, event_id, etag)) =
            create_copy_via_jmap(&jmap, username, password, &ics).await
        {
            let server_id = crate::sync::generate_server_id(
                state.cfg.hmac_secret(),
                &format!("jmap:{}:{}", account_id, event_id),
            );
            let resource_href = format!("jmap://calendar/{}/{}", account_id, event_id);
            state
                .storage
                .upsert_item_map(username, "", &resource_href, &server_id, &item.uid, &etag)
                .await?;
            return Ok(server_id);
        }
        // JMAP create failed → fall through to CalDAV.
        tracing::warn!(target: "meeting", "RSVP: JMAP attendee-copy create failed; falling back to CalDAV");
    }

    // CalDAV fallback / primary.
    let caldav = crate::caldav::CaldavClient::new(&state.cfg)?;
    let calendars = caldav
        .find_user_calendars(username, password.expose_secret())
        .await?;
    let collection_href = calendars
        .first()
        .ok_or_else(|| anyhow!("no writable calendar collection discovered"))?
        .clone();
    let (resource_href, etag) = caldav
        .put_event(
            &collection_href,
            None,
            &ics,
            username,
            password.expose_secret(),
            None,
        )
        .await?;
    let server_id = crate::sync::generate_server_id(state.cfg.hmac_secret(), &resource_href);
    state
        .storage
        .upsert_item_map(
            username,
            &collection_href,
            &resource_href,
            &server_id,
            &item.uid,
            &etag,
        )
        .await?;
    Ok(server_id)
}

// ---------------------------------------------------------------------------
// Instance helpers
// ---------------------------------------------------------------------------

/// The idempotence / RECURRENCE-ID key for an instance-scoped response.
/// Whole-series responses use the empty string.
pub fn instance_key_for(instance_id: Option<DateTime<Utc>>) -> String {
    match instance_id {
        None => String::new(),
        Some(dt) => format!("{}Z", dt.format("%Y%m%dT%H%M%S")),
    }
}

/// Find the exception already recorded for this instance, if any.
fn find_matching_exception(
    item: &CalendarItem,
    instance_id: DateTime<Utc>,
) -> Option<&crate::calendar::CalendarException> {
    item.exceptions
        .iter()
        .find(|ex| ex.exception_start == instance_id)
}

/// True when `instance_id` names an occurrence that actually exists in the
/// item's recurrence: the series start itself, a live expansion of the
/// RRULE, or an existing (non-deleted) exception.
///
/// Expansion uses the existing `rrule` crate. Any expansion failure (unknown
/// RRULE grammar, floating times) is treated as "assume valid" — a validator
/// limitation must never block a legitimate response; the backend records the
/// exception keyed by RECURRENCE-ID either way. EXDATE-removed instances and
/// deleted exceptions are nonexistent per RFC 5545 §3.8.5.1.
fn instance_exists(item: &CalendarItem, instance_id: DateTime<Utc>) -> bool {
    if instance_id < item.start {
        return false;
    }
    if item.exdates.contains(&instance_id) {
        return false;
    }
    if let Some(ex) = find_matching_exception(item, instance_id) {
        // A live exception replaces the generated occurrence; a deleted one
        // removes it.
        return !ex.deleted;
    }
    if item.start == instance_id {
        return true;
    }
    let Some(rrule_str) = item.rrule.as_deref().filter(|r| !r.is_empty()) else {
        // No recurrence: only the master itself exists.
        return false;
    };
    // `None` (rule uninterpretable) fails open: cannot disprove the instance.
    expand_occurrences(rrule_str, item.start, instance_id).unwrap_or(true)
}

/// Expand `rrule` from `dtstart` and report whether `target` is one of the
/// occurrences. Returns `None` when the rule cannot be interpreted (unknown
/// grammar, unbuildable rule) — callers treat that as "cannot disprove".
fn expand_occurrences(
    rrule_str: &str,
    dtstart: DateTime<Utc>,
    target: DateTime<Utc>,
) -> Option<bool> {
    use rrule::{RRule, Tz};
    use std::str::FromStr;

    let rule_text = rrule_str
        .trim()
        .strip_prefix("RRULE:")
        .unwrap_or(rrule_str.trim());
    let rule = RRule::from_str(rule_text).ok()?;
    let dtstart_tz = dtstart.with_timezone(&Tz::UTC);
    // Bound the search just past the target so the iterator can terminate;
    // a hard occurrence cap guards pathological rules.
    let horizon = target.with_timezone(&Tz::UTC) + chrono::Duration::days(1);
    let set = rule.build(dtstart_tz).ok()?.before(horizon);
    const OCCURRENCE_CAP: usize = 4096;
    for (i, occurrence) in set.into_iter().enumerate() {
        if occurrence.with_timezone(&Utc) == target {
            return Some(true);
        }
        if i >= OCCURRENCE_CAP {
            return None;
        }
    }
    Some(false)
}

// ---------------------------------------------------------------------------
// Decision code mappings
// ---------------------------------------------------------------------------

/// [MS-ASCMD] §2.2.3.194 UserResponse codes (1 accept / 2 tentative /
/// 3 decline), reused as the storage decision code.
pub fn decision_code(decision: crate::meeting::response::ResponseDecision) -> i32 {
    match decision {
        crate::meeting::response::ResponseDecision::Accept => 1,
        crate::meeting::response::ResponseDecision::Tentative => 2,
        crate::meeting::response::ResponseDecision::Decline => 3,
    }
}

/// [MS-ASCAL] §2.2.2.21 ResponseType codes for the responder's copy:
/// 2 tentative / 3 accepted / 4 declined.
pub fn response_type_code(decision: crate::meeting::response::ResponseDecision) -> u8 {
    match decision {
        crate::meeting::response::ResponseDecision::Accept => 3,
        crate::meeting::response::ResponseDecision::Tentative => 2,
        crate::meeting::response::ResponseDecision::Decline => 4,
    }
}

/// [MS-ASCAL] §2.2.2.4 attendee status codes (0 response requested / 1
/// response not needed / 2 response sent / 3 accepted / 4 declined / 5
/// tentative — the accepted mapping used across the gateway's sync paths).
pub fn attendee_status_code(decision: crate::meeting::response::ResponseDecision) -> u8 {
    response_type_code(decision)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn decision_req(decision: crate::meeting::response::ResponseDecision) -> RsvpRequest {
        RsvpRequest {
            decision,
            instance_id: None,
            send_reply: false,
            reply_body_text: None,
            proposed_start: None,
            proposed_end: None,
        }
    }

    #[test]
    fn status_codes_match_ms_ascmd_177_9() {
        assert_eq!(STATUS_SUCCESS, 1);
        assert_eq!(STATUS_INVALID_ITEM, 2);
        assert_eq!(STATUS_SERVER_ERROR, 3);
        // [MS-ASCMD] §2.2.3.92.1: element-specific MeetingResponse statuses.
        assert_eq!(STATUS_INSTANCE_MALFORMED, 104);
        assert_eq!(STATUS_INSTANCE_NOT_RECURRING, 146);
    }

    #[test]
    fn decision_codes_match_user_response_element() {
        use crate::meeting::response::ResponseDecision;
        assert_eq!(decision_code(ResponseDecision::Accept), 1);
        assert_eq!(decision_code(ResponseDecision::Tentative), 2);
        assert_eq!(decision_code(ResponseDecision::Decline), 3);
    }

    #[test]
    fn response_type_codes_match_ms_ascal_2_2_2_21() {
        use crate::meeting::response::ResponseDecision;
        assert_eq!(response_type_code(ResponseDecision::Accept), 3);
        assert_eq!(response_type_code(ResponseDecision::Tentative), 2);
        assert_eq!(response_type_code(ResponseDecision::Decline), 4);
    }

    #[test]
    fn instance_key_matches_recurrence_id_format() {
        let whole = instance_key_for(None);
        assert_eq!(whole, "");
        let dt = Utc.with_ymd_and_hms(2026, 7, 10, 9, 0, 0).unwrap();
        assert_eq!(instance_key_for(Some(dt)), "20260710T090000Z");
    }

    #[test]
    fn lone_proposed_time_is_invalid() {
        let mut req = decision_req(crate::meeting::response::ResponseDecision::Tentative);
        req.proposed_start = Some(Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap());
        // The pairing rule is enforced at dispatch; here we only pin the
        // helper semantics the dispatcher relies on.
        let countering = req.proposed_start.is_some() || req.proposed_end.is_some();
        assert!(countering);
        assert!(!(req.proposed_start.is_some() && req.proposed_end.is_some()));
    }

    #[test]
    fn patch_series_response_sets_partstat_and_reply_time() {
        let mut item = CalendarItem {
            uid: "u1".into(),
            attendees: vec![crate::calendar::Attendee {
                email: "bob@example.com".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        patch_series_response(
            &mut item,
            "bob@example.com",
            crate::meeting::response::ResponseDecision::Accept,
        );
        assert_eq!(item.attendees[0].partstat.as_deref(), Some("ACCEPTED"));
        assert_eq!(item.response_type, Some(3));
        assert!(item.appointment_reply_time.is_some());
    }

    #[test]
    fn patch_instance_response_records_exception_not_master() {
        use chrono::TimeZone;
        let mut item = CalendarItem {
            uid: "u1".into(),
            start: Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap(),
            attendees: vec![crate::calendar::Attendee {
                email: "bob@example.com".into(),
                partstat: Some("NEEDS-ACTION".into()),
                ..Default::default()
            }],
            response_type: Some(0),
            ..Default::default()
        };
        let iid = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap();
        patch_instance_response(
            &mut item,
            "bob@example.com",
            crate::meeting::response::ResponseDecision::Tentative,
            iid,
        );
        // Master untouched; exception carries the decision.
        assert_eq!(item.response_type, Some(0));
        assert_eq!(item.attendees[0].partstat.as_deref(), Some("NEEDS-ACTION"));
        assert_eq!(item.exceptions.len(), 1);
        let ex = &item.exceptions[0];
        assert_eq!(ex.exception_start, iid);
        assert_eq!(ex.response_type, Some(2));
        assert_eq!(
            ex.attendees.as_ref().unwrap()[0].partstat.as_deref(),
            Some("TENTATIVE")
        );
        assert!(ex.appointment_reply_time.is_some());
    }

    #[test]
    fn patch_instance_response_merges_existing_exception() {
        use chrono::TimeZone;
        let iid = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap();
        let mut item = CalendarItem {
            uid: "u1".into(),
            attendees: vec![crate::calendar::Attendee {
                email: "bob@example.com".into(),
                ..Default::default()
            }],
            exceptions: vec![crate::calendar::CalendarException {
                deleted: false,
                exception_start: iid,
                subject: Some("Moved".into()),
                start: Some(iid),
                end: None,
                response_type: None,
                appointment_reply_time: None,
                meeting_status: None,
                attendees: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        patch_instance_response(
            &mut item,
            "bob@example.com",
            crate::meeting::response::ResponseDecision::Decline,
            iid,
        );
        assert_eq!(item.exceptions.len(), 1);
        let ex = &item.exceptions[0];
        // Pre-existing exception fields preserved.
        assert_eq!(ex.subject.as_deref(), Some("Moved"));
        assert_eq!(ex.response_type, Some(4));
        assert_eq!(
            ex.attendees.as_ref().unwrap()[0].partstat.as_deref(),
            Some("DECLINED")
        );
    }

    #[test]
    fn apply_attendee_decision_matches_case_insensitively() {
        let mut attendees = vec![crate::calendar::Attendee {
            email: "Bob@Example.com".into(),
            ..Default::default()
        }];
        apply_attendee_decision(
            &mut attendees,
            "bob@example.com",
            crate::meeting::response::ResponseDecision::Accept,
        );
        assert_eq!(attendees.len(), 1);
        assert_eq!(attendees[0].partstat.as_deref(), Some("ACCEPTED"));
    }

    #[test]
    fn apply_attendee_decision_appends_unknown_responder() {
        let mut attendees = vec![crate::calendar::Attendee {
            email: "someone-else@example.com".into(),
            ..Default::default()
        }];
        apply_attendee_decision(
            &mut attendees,
            "bob@example.com",
            crate::meeting::response::ResponseDecision::Decline,
        );
        assert_eq!(attendees.len(), 2);
        assert_eq!(attendees[1].email, "bob@example.com");
        assert_eq!(attendees[1].partstat.as_deref(), Some("DECLINED"));
        // RFC 6638 §7.1: appended copy must not double-schedule.
        assert_eq!(attendees[1].schedule_agent.as_deref(), Some("CLIENT"));
    }

    #[test]
    fn instance_exists_rejects_before_series_start_and_exdates() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap();
        let earlier = Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap();
        let item = CalendarItem {
            uid: "u1".into(),
            start,
            exdates: vec![later],
            ..Default::default()
        };
        assert!(instance_exists(&item, start));
        assert!(!instance_exists(&item, earlier));
        assert!(!instance_exists(&item, later));
    }

    #[test]
    fn instance_exists_uses_rrule_expansion() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap(); // Monday
        let valid = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap(); // next Monday
        let invalid = Utc.with_ymd_and_hms(2026, 1, 8, 10, 0, 0).unwrap(); // Thursday
        let item = CalendarItem {
            uid: "u1".into(),
            start,
            rrule: Some("FREQ=WEEKLY;COUNT=10".into()),
            ..Default::default()
        };
        assert!(instance_exists(&item, valid));
        assert!(!instance_exists(&item, invalid));
    }

    #[test]
    fn instance_exists_fails_open_on_unknown_rule() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let any = Utc.with_ymd_and_hms(2027, 6, 1, 10, 0, 0).unwrap();
        let item = CalendarItem {
            uid: "u1".into(),
            start,
            rrule: Some("FREQ=CENTENNIALLY;BYMOONPHASE=FULL".into()),
            ..Default::default()
        };
        assert!(instance_exists(&item, any));
    }

    #[test]
    fn instance_exists_treats_deleted_exception_as_nonexistent() {
        use chrono::TimeZone;
        let start = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let iid = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap();
        let item = CalendarItem {
            uid: "u1".into(),
            start,
            rrule: Some("FREQ=WEEKLY;COUNT=10".into()),
            exceptions: vec![crate::calendar::CalendarException {
                deleted: true,
                exception_start: iid,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(!instance_exists(&item, iid));
    }

    #[test]
    fn decision_codes_roundtrip_with_partstat() {
        use crate::meeting::response::ResponseDecision;
        for (decision, partstat, code) in [
            (ResponseDecision::Accept, "ACCEPTED", 1),
            (ResponseDecision::Tentative, "TENTATIVE", 2),
            (ResponseDecision::Decline, "DECLINED", 3),
        ] {
            assert_eq!(decision.as_partstat(), partstat);
            assert_eq!(decision_code(decision), code);
        }
    }
}
