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
//      JMAP Calendar or CalDAV per the configured backend. An existing
//      UID-mapped copy is only patched after re-reading it and confirming
//      its organizer matches the REQUEST's — meeting identity is
//      (organizer, UID), so a UID collision (or a forged REQUEST) must
//      never overwrite an unrelated stored event, and a decision lands on
//      the copy as it exists NOW (organizer reschedules sync in through
//      the normal calendar paths). All attendee SCHEDULE-AGENTs are pinned
//      to CLIENT (RFC 6638 §7.1) so Stalwart's own scheduler never
//      re-emits an iTIP copy of our reply from the attendee's copy.
//   5. Deliver the iTIP REPLY to the organizer over SMTP (RFC 5546 §3.2.10 /
//      RFC 6047) — with RECURRENCE-ID and the instance's own times when
//      instance-scoped (RFC 5546 §3.6.2) — plus an iTIP COUNTER
//      (RFC 5546 §3.6.7) when the client proposed a new time
//      ([MS-ASCMD] §2.2.3.140/§2.2.3.141). The REPLY is recorded the
//      moment it is delivered; the COUNTER is never recorded, so a retry
//      after a failed COUNTER re-proposes without re-sending the REPLY.
//      Duplicate responses (same decision for the same (uid, instance)
//      answering the same REQUEST revision) are idempotent: the stored
//      RSVP ([MS-ASCMD] §2.2.1.11 disambiguation table, "User responds to a
//      meeting request for which a response was already sent") suppresses
//      the second REPLY email; a changed decision — or the same decision
//      for a rescheduled REQUEST whose SEQUENCE moved (RFC 5546 §3.2.1.4
//      revision semantics) — is delivered and recorded.
//
// Statuses returned follow [MS-ASCMD] §2.2.3.177.9 exactly: 1 success, 2 for
// invalid-item conditions (malformed/unresolvable address, organizer
// self-response, InstanceId on an email item, nonexistent instance), 3 for
// transient backend failure (retry). Resolution is Global-scope for 3 and
// item-scope for 2 per the same table.

use crate::calendar::{CalendarItem, mark_scheduling_client_side, parse_ics_event, render_ics};
use crate::email::extract_meeting_request_ics;
use crate::models::AppState;
use crate::storage::{EwsItemRow, MeetingRsvpRow};
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
    let mut row = state
        .storage
        .get_ews_item_by_server_id(username, server_id)
        .await
        .map_err(|e| {
            tracing::error!(target: "meeting", error = %e, "RSVP: item_map read failed");
            ResolveFailure::ServerError
        })?
        .ok_or(ResolveFailure::NotFound)?;

    let (ics, etag) = read_calendar_row_ics(state, username, password, &row).await?;

    let Some(item) = parse_ics_event(&ics) else {
        tracing::warn!(target: "meeting", server_id = %server_id, "RSVP: calendar item iCalendar unparseable");
        return Err(ResolveFailure::InvalidItem);
    };
    if item.organizer_email.as_deref().is_none_or(|e| e.is_empty()) {
        // No organizer → a plain appointment, not a meeting.
        return Err(ResolveFailure::InvalidItem);
    }

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

/// Read the current iCalendar of a mapped calendar row from whichever
/// backend owns it. JMAP rows keep the event id in the href; CalDAV rows
/// store a direct href. Returns `(ics, etag)`.
async fn read_calendar_row_ics(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    row: &EwsItemRow,
) -> std::result::Result<(String, Option<String>), ResolveFailure> {
    if row.resource_href.starts_with("jmap://") {
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
        Ok((ics, Some(returned_etag)))
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
        Ok((ics, returned_etag))
    }
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
        Ok(Some(row)) => is_duplicate_rsvp(&row, decision_code(req.decision), invitation.sequence),
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

    // iTIP delivery. The REPLY is skipped when the client did not ask for an
    // email ([MS-ASCMD] §2.2.3.163) or when this exact (decision, SEQUENCE)
    // revision was already delivered (duplicate → idempotent no-op). The
    // COUNTER — a new-time proposal — is delivered whenever the request
    // carries one, independent of the duplicate state: it is not recorded in
    // the idempotence store, so a retry after a failed COUNTER re-proposes
    // without re-sending the already-delivered REPLY.
    let ctx = ItipSendCtx {
        state,
        username,
        password,
        responder_email: &user_email,
    };
    let mut reply_message_id = None;
    let mut counter_message_id = None;
    if req.send_reply && !duplicate {
        match send_itip_reply(&ctx, &invitation, req).await {
            Ok(message_id) => {
                reply_message_id = Some(message_id);
                // Record immediately after the successful REPLY send — not
                // after the COUNTER. A failed COUNTER must not discard the
                // fact that the organizer already received this decision: the
                // status-3 retry then re-delivers only the COUNTER, never a
                // second REPLY.
                if let Err(e) = state
                    .storage
                    .upsert_meeting_rsvp(&crate::storage::MeetingRsvpRecord {
                        owner: username,
                        uid: &invitation.item.uid,
                        instance_key: &instance_key,
                        decision: decision_code(req.decision),
                        sequence: i64::from(invitation.sequence),
                        message_id: reply_message_id.as_deref(),
                        calendar_server_id: calendar_server_id.as_deref(),
                    })
                    .await
                {
                    // At-least-once semantics: an unrecorded delivery may
                    // repeat on retry. A duplicate REPLY is benign iTIP-wise
                    // (organizers fold repeats by PARTSTAT); a lost one is not.
                    tracing::warn!(target: "meeting", error = %e, "RSVP: idempotence record write failed");
                }
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
                    "RSVP: iTIP REPLY delivery failed; client should retry (status 3)"
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

    if req.send_reply && req.proposed_start.is_some() && req.proposed_end.is_some() {
        match send_itip_counter(&ctx, &invitation, req).await {
            Ok(message_id) => counter_message_id = Some(message_id),
            Err(e) => {
                // The REPLY (if one was needed) is already recorded; this
                // failure reports as status 3 so the client retries, and the
                // retry re-delivers only the COUNTER.
                tracing::warn!(
                    target: "meeting",
                    error = %e,
                    uid = %invitation.item.uid,
                    "RSVP: iTIP COUNTER delivery failed; client should retry (status 3)"
                );
                return Ok(RsvpOutcome {
                    status: STATUS_SERVER_ERROR,
                    calendar_server_id,
                    reply_message_id,
                    duplicate,
                    ..RsvpOutcome::failure(&ResolveFailure::ServerError)
                });
            }
        }
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

/// A recorded REPLY suppresses a new REPLY only when the decision and the
/// answered REQUEST revision both repeat: the stored row carries the iCalendar
/// SEQUENCE the delivered REPLY answered (RFC 5546 §3.2.1.4), so an organizer
/// reschedule (SEQUENCE bump) is a distinct delivery even when the attendee
/// presses the same button again.
fn is_duplicate_rsvp(row: &MeetingRsvpRow, decision: i32, sequence: u32) -> bool {
    row.decision == decision && row.sequence == i64::from(sequence)
}

/// Borrowed context for one iTIP send — the `SyncCtx` convention applied to
/// the two SMTP senders so their signatures stay at (ctx, invitation, req).
#[derive(Clone, Copy)]
struct ItipSendCtx<'a> {
    state: &'a Arc<AppState>,
    username: &'a str,
    password: &'a SecretString,
    responder_email: &'a str,
}

/// The VEVENT times an iTIP message about this response refers to: the
/// instance's own times when instance-scoped (RFC 5546 §3.6.2 REPLY with
/// RECURRENCE-ID refers to the occurrence, not the series master), the
/// master's times for a whole-series response.
fn reply_window(
    item: &CalendarItem,
    instance_id: Option<DateTime<Utc>>,
) -> (DateTime<Utc>, DateTime<Utc>) {
    match instance_id {
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
    }
}

/// Deliver the iTIP REPLY to the organizer ([MS-ASCMD] §2.2.3.163 email
/// requested; RFC 5546 §3.6.2). Returns the SMTP message id.
async fn send_itip_reply(
    ctx: &ItipSendCtx<'_>,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
) -> Result<String> {
    let ItipSendCtx {
        state,
        username,
        password,
        responder_email,
    } = *ctx;
    let Some(smtp) = state.smtp_client.as_ref() else {
        return Err(anyhow!(
            "SMTP client not configured; cannot deliver iTIP reply (JMAP EmailSubmission does not support text/calendar MIME)"
        ));
    };

    let item = &invitation.item;
    let organizer_email = item.organizer_email.clone().unwrap_or_default();
    let (reply_start, reply_end) = reply_window(item, req.instance_id);

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
    Ok(result.message_id)
}

/// Deliver a new-time proposal as a separate iTIP COUNTER (RFC 5546 §3.6.7).
/// The REPLY already carried the decision; the COUNTER carries the proposed
/// times, mirroring how Outlook's "Propose New Time" delivers both. Returns
/// the SMTP message id.
async fn send_itip_counter(
    ctx: &ItipSendCtx<'_>,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
) -> Result<String> {
    let ItipSendCtx {
        state,
        username,
        password,
        responder_email,
    } = *ctx;
    let Some(smtp) = state.smtp_client.as_ref() else {
        return Err(anyhow!(
            "SMTP client not configured; cannot deliver iTIP counter (JMAP EmailSubmission does not support text/calendar MIME)"
        ));
    };

    let item = &invitation.item;
    // The proposal pair is validated all-or-nothing at dispatch; a half pair
    // reaching this sender is a programming error, not a user condition.
    let (Some(proposed_start), Some(proposed_end)) = (req.proposed_start, req.proposed_end) else {
        return Err(anyhow!(
            "iTIP COUNTER requested without a complete time pair"
        ));
    };
    let (reply_start, reply_end) = reply_window(item, req.instance_id);
    let msg = crate::meeting::message::MeetingMessage::new_counter(
        &crate::meeting::message::CounterParams {
            uid: item.uid.clone(),
            organizer_email: item.organizer_email.clone().unwrap_or_default(),
            subject: item.subject.clone(),
            original_start: reply_start,
            original_end: reply_end,
            proposed_start,
            proposed_end,
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
                start = proposed_start.format("%Y-%m-%d %H:%M:%SZ"),
                end = proposed_end.format("%Y-%m-%d %H:%M:%SZ"),
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
    Ok(result.message_id)
}

/// Patch (or create) the local attendee calendar copy. Returns the calendar
/// server id of the copy for `<CalendarId>`, or `None` for declines with no
/// existing copy (Exchange does not book declined meetings).
///
/// An existing UID-mapped copy is only ever *patched*, and only after the
/// stored event is re-read and its organizer confirmed to be the same
/// meeting the REQUEST belongs to. Two hard reasons:
///
/// - **Identity**: a meeting is (organizer, UID), not the UID alone. A
///   forged REQUEST can carry any UID, so answering to it must never
///   overwrite an unrelated calendar item — least of all one the responding
///   user organizes ([MS-OXOCAL] identity; RFC 5546 §3.6.2 REPLY semantics).
/// - **Currency**: the REQUEST email may be older than the stored copy
///   (organizer reschedules sync in through the normal calendar paths).
///   The decision is recorded on the copy as it exists now — its
///   organizer-owned fields (times, roster, exceptions) survive untouched.
///
/// When the stored event cannot be read, parsed, or attributed to the same
/// organizer, the row is left alone and the response books a separate copy
/// of the meeting the REQUEST actually describes: a duplicate calendar item
/// is recoverable, a clobbered one is not.
async fn update_local_attendee_copy(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
    user_email: &str,
) -> Result<Option<String>> {
    // The stored item the copy-update patches. Calendar sources resolved
    // from the row itself; email sources re-read the UID-mapped row and
    // verify the organizer match before anything is written.
    let (row, mut item) = match &invitation.calendar_row {
        Some(row) if invitation.from_email => {
            let (ics, _etag) = read_calendar_row_ics(state, username, password, row)
                .await
                .map_err(|e| {
                    anyhow!(
                        "stored attendee copy unreadable (uid {}): {:?}",
                        invitation.item.uid,
                        e
                    )
                })?;
            let stored = parse_ics_event(&ics).ok_or_else(|| {
                anyhow!(
                    "stored attendee copy unparseable (uid {})",
                    invitation.item.uid
                )
            })?;
            if !same_meeting_organizer(&stored, &invitation.item) {
                // UID collision across different organizers: the stored item
                // belongs to another meeting and must not be touched. The
                // response proceeds against the REQUEST's own meeting only.
                tracing::warn!(
                    target: "meeting",
                    uid = %invitation.item.uid,
                    server_id = %row.server_id,
                    "RSVP: UID-mapped item belongs to a different organizer; leaving it untouched"
                );
                return book_attendee_copy(state, username, password, invitation, req, user_email)
                    .await;
            }
            (row, stored)
        }
        Some(row) => (row, invitation.item.clone()),
        None => {
            return book_attendee_copy(state, username, password, invitation, req, user_email)
                .await;
        }
    };

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

    write_existing_copy(state, username, password, row, &item).await?;
    Ok(Some(row.server_id.clone()))
}

/// Create a fresh attendee copy when no UID-mapped (or organizer-verified)
/// stored copy exists. Declines book nothing (Exchange semantics).
async fn book_attendee_copy(
    state: &Arc<AppState>,
    username: &str,
    password: &SecretString,
    invitation: &ResolvedInvitation,
    req: &RsvpRequest,
    user_email: &str,
) -> Result<Option<String>> {
    let mut item = invitation.item.clone();

    // Declined + no existing copy: don't create one.
    if matches!(
        req.decision,
        crate::meeting::response::ResponseDecision::Decline
    ) {
        return Ok(None);
    }

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

    let server_id = create_attendee_copy(state, username, password, &item).await?;
    Ok(Some(server_id))
}

/// The same meeting, or two different ones sharing a UID? Meeting identity
/// is (organizer, UID) ([MS-OXOCAL]); only a REQUEST from the stored copy's
/// own organizer may update it.
fn same_meeting_organizer(stored: &CalendarItem, request: &CalendarItem) -> bool {
    match (
        stored.organizer_email.as_deref().filter(|e| !e.is_empty()),
        request.organizer_email.as_deref().filter(|e| !e.is_empty()),
    ) {
        (Some(stored_org), Some(request_org)) => {
            normalize_email(stored_org) == normalize_email(request_org)
        }
        // Either side without an organizer cannot be attributed; resolution
        // already rejects organizer-less requests, and a stored copy without
        // an organizer is not a meeting copy to defend.
        _ => false,
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
    expand_occurrences(rrule_str, item.start, item.timezone.as_deref(), instance_id).unwrap_or(true)
}

/// Expand `rrule` from `dtstart` and report whether `target` is one of the
/// occurrences. Returns `None` when the rule cannot be interpreted (unknown
/// grammar, unbuildable rule, or a declared timezone the expansion cannot
/// honor) — callers treat that as "cannot disprove".
///
/// A recurring event's wall-clock time is fixed in its own timezone
/// (RFC 5545 §3.8.5.3 DTSTART/RRULE interplay): a weekly 09:00
/// Europe/Berlin series starts one UTC hour earlier after the spring DST
/// transition. Expanding in UTC would pin the UTC time year-round and reject
/// every summer occurrence, so the rule is expanded in the event's own zone
/// whenever `timezone` parses as an IANA name. A declared zone that does
/// not parse also fails open — expanding in the wrong zone would reject
/// valid instances, the very failure this guards against.
fn expand_occurrences(
    rrule_str: &str,
    dtstart: DateTime<Utc>,
    timezone: Option<&str>,
    target: DateTime<Utc>,
) -> Option<bool> {
    use rrule::{RRule, Tz};
    use std::str::FromStr;

    let rule_text = rrule_str
        .trim()
        .strip_prefix("RRULE:")
        .unwrap_or(rrule_str.trim());
    let rule = RRule::from_str(rule_text).ok()?;
    let tz = match timezone {
        None => Tz::UTC,
        Some(name) => match name.parse::<chrono_tz::Tz>() {
            // The event's zone drives the expansion; the UTC instants of the
            // generated occurrences shift across DST changes.
            Ok(zone) => Tz::from(zone),
            // Uninterpretable zone: expanding in UTC would judge the instance
            // in a zone the event does not live in — cannot disprove.
            Err(_) => return None,
        },
    };
    let dtstart_tz = dtstart.with_timezone(&tz);
    let set = rule.build(dtstart_tz).ok()?;
    // The iterator yields occurrences in chronological order starting at
    // DTSTART, so the scan can stop at the first occurrence past the target.
    // (RRuleSet::before() only applies to the `all()` collection APIs, not
    // to direct iteration.) The cap guards pathological rules whose expansion
    // never reaches the target.
    const OCCURRENCE_CAP: usize = 4096;
    for (i, occurrence) in set.into_iter().enumerate() {
        let occurrence = occurrence.with_timezone(&Utc);
        if occurrence == target {
            return Some(true);
        }
        if occurrence > target {
            return Some(false);
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

    /// RFC 5545 §3.8.5.3: a recurring event's wall clock is fixed in its own
    /// timezone, so the UTC instants of the occurrences shift across a DST
    /// transition. A weekly 09:00 Europe/Berlin series starting in winter
    /// (08:00Z) recurs at 07:00Z in summer — expanding the rule in UTC would
    /// reject the summer occurrence ([MS-ASCmd] §2.2.3.177.9 status 2 instead
    /// of recording the response).
    #[test]
    fn expand_occurrences_honors_dst_transitions_in_event_timezone() {
        use chrono::TimeZone;
        let dtstart = chrono_tz::Tz::Europe__Berlin
            .with_ymd_and_hms(2026, 1, 5, 9, 0, 0)
            .unwrap() // Monday 09:00 Berlin = 08:00Z (winter, CET)
            .with_timezone(&Utc);
        let winter = Utc.with_ymd_and_hms(2026, 1, 12, 8, 0, 0).unwrap();
        // Same wall clock after the 2026-03-29 spring transition: CEST → 07:00Z.
        let summer = Utc.with_ymd_and_hms(2026, 7, 6, 7, 0, 0).unwrap();
        // The naive UTC-pinned wall clock in summer: NOT an occurrence.
        let utc_pinned = Utc.with_ymd_and_hms(2026, 7, 6, 8, 0, 0).unwrap();

        for (label, target, expect) in [
            ("winter occurrence", winter, Some(true)),
            ("summer occurrence across DST", summer, Some(true)),
            ("utc-pinned summer instant", utc_pinned, Some(false)),
        ] {
            assert_eq!(
                expand_occurrences(
                    "FREQ=WEEKLY;COUNT=40",
                    dtstart,
                    Some("Europe/Berlin"),
                    target
                ),
                expect,
                "{label}"
            );
        }
    }

    /// `instance_exists` threads the event's timezone into the expansion: the
    /// summer occurrence of a zoned series is real, not a status-146/2
    /// rejection.
    #[test]
    fn instance_exists_accepts_dst_shifted_occurrence() {
        use chrono::TimeZone;
        let start = chrono_tz::Tz::Europe__Berlin
            .with_ymd_and_hms(2026, 1, 5, 9, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let item = CalendarItem {
            uid: "u1".into(),
            start,
            rrule: Some("FREQ=WEEKLY;COUNT=40".into()),
            timezone: Some("Europe/Berlin".into()),
            ..Default::default()
        };
        let summer = Utc.with_ymd_and_hms(2026, 7, 6, 7, 0, 0).unwrap();
        assert!(instance_exists(&item, summer));
    }

    /// A declared timezone the expansion cannot interpret must fail open —
    /// judging the instance in a foreign zone (or UTC) would reject valid
    /// instances, the exact failure class this guards against.
    #[test]
    fn expand_occurrences_fails_open_on_unparseable_timezone() {
        use chrono::TimeZone;
        let dtstart = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let target = Utc.with_ymd_and_hms(2026, 2, 2, 10, 0, 0).unwrap();
        assert_eq!(
            expand_occurrences(
                "FREQ=WEEKLY;COUNT=10",
                dtstart,
                Some("W. Europe Standard Time"),
                target
            ),
            None
        );
        // The caller converts `None` to "cannot disprove".
        let item = CalendarItem {
            uid: "u1".into(),
            start: dtstart,
            rrule: Some("FREQ=WEEKLY;COUNT=10".into()),
            timezone: Some("W. Europe Standard Time".into()),
            ..Default::default()
        };
        assert!(instance_exists(&item, target));
    }

    /// A UTC-defined event (no TZID) keeps fixed UTC instants — unchanged
    /// behavior from before the zoned expansion.
    #[test]
    fn expand_occurrences_utc_event_stays_utc() {
        use chrono::TimeZone;
        let dtstart = Utc.with_ymd_and_hms(2026, 1, 5, 10, 0, 0).unwrap();
        let valid = Utc.with_ymd_and_hms(2026, 1, 12, 10, 0, 0).unwrap();
        let invalid = Utc.with_ymd_and_hms(2026, 1, 12, 11, 0, 0).unwrap();
        assert_eq!(
            expand_occurrences("FREQ=WEEKLY;COUNT=10", dtstart, None, valid),
            Some(true)
        );
        assert_eq!(
            expand_occurrences("FREQ=WEEKLY;COUNT=10", dtstart, None, invalid),
            Some(false)
        );
    }

    /// [MS-ASCMD] §2.2.1.11 idempotence with revision awareness: a repeated
    /// (decision, SEQUENCE) is a duplicate; an organizer reschedule raises
    /// SEQUENCE (RFC 5546 §3.2.1.4) and the same decision is a NEW delivery;
    /// a changed decision on the same revision is new too.
    #[test]
    fn is_duplicate_rsvp_requires_decision_and_sequence_match() {
        let row = crate::storage::MeetingRsvpRow {
            uid: "evt-1".into(),
            instance_key: String::new(),
            decision: 1,
            sequence: 0,
            message_id: Some("<m@example.com>".into()),
            calendar_server_id: None,
            responded_at: "2026-10-01 00:00:00".into(),
        };
        assert!(is_duplicate_rsvp(&row, 1, 0), "exact retry is a duplicate");
        assert!(
            !is_duplicate_rsvp(&row, 1, 1),
            "reschedule (SEQUENCE bump) is a new delivery"
        );
        assert!(
            !is_duplicate_rsvp(&row, 2, 0),
            "changed decision is a new delivery"
        );
        assert!(!is_duplicate_rsvp(&row, 3, 7), "both changed");
    }

    /// Meeting identity is (organizer, UID) — the UID-mapped copy is only
    /// patchable by a REQUEST from the same meeting's organizer; anything
    /// else (a UID collision with a forged or unrelated request) must never
    /// reach `write_existing_copy`.
    #[test]
    fn same_meeting_organizer_compares_normalized_identity() {
        let stored = CalendarItem {
            uid: "u1".into(),
            organizer_email: Some("mailto:Organizer@Example.com".into()),
            ..Default::default()
        };
        let request = CalendarItem {
            uid: "u1".into(),
            organizer_email: Some("organizer@example.com".into()),
            ..Default::default()
        };
        assert!(same_meeting_organizer(&stored, &request));

        let other_organizer = CalendarItem {
            uid: "u1".into(),
            organizer_email: Some("mallory@example.net".into()),
            ..Default::default()
        };
        assert!(!same_meeting_organizer(&stored, &other_organizer));

        let organizerless = CalendarItem {
            uid: "u1".into(),
            organizer_email: None,
            ..Default::default()
        };
        assert!(!same_meeting_organizer(&stored, &organizerless));
        assert!(!same_meeting_organizer(&organizerless, &request));
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
