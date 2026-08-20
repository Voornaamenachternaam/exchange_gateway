// src/mapi/handler.rs
//
// The MAPI/HTTP request orchestrator: takes a parsed `MapiRequest` from
// `transport.rs`, dispatches by X-RequestType, and produces the
// `MapiResponse` the axum route renders.
//
// Phase 0 dispatch:
//   * Connect    ŌĆö parse the ROP buffer, dispatch the leading ROP (which
//                  must be RopLogon for a fresh connection), authenticate via
//                  Basic auth, allocate a session, encode the RopLogonSuccess.
//   * Execute    ŌĆö look up the session (by the X-ClientInfo / X-Connection
//                  cookie path), parse the requested RopId, and delegate to
//                  the matching ROP handler. Phase 0 implements a stable
//                  dispatch table for the Phase-0 ROP set; unknown ROPs and
//                  the address-book-only ROP set return a typed error
//                  envelope (code 5 / InvalidRequestType at the transport
//                  layer; for an in-session Execute the ROP-level envelope
//                  returns InvalidParameter).
//   * Disconnect ŌĆö drop the named session and return code 0.
//   * NotificationWait / PING ŌĆö Phase 0 returns a deterministic
//                  success-with-empty-body response (the long-poll behaviour
//                  lands in Phase 1).
//
// All decode paths fail closed: an unrecoverable buffer under-run or an
// unexpected RopId on a `Connect` returns a transport-layer
// `ResponseCode::InvalidRequestBody` (12).

use crate::auth::AuthVerifier;
use crate::config::Config;
use crate::mapi::logon::{LogonOutcome, logon_basic};
// `secrecy::SecretString` parameters carry the password across the ROP
// dispatch boundary; `ExposeSecret` is required to call `expose_secret()`
// for the CalDAV/CardDAV `basic_auth` password (which take a plain `&str`).
use crate::mapi::rops::{
    Buf, DecodeError, RopCommitStreamRequest, RopCommitStreamResponse, RopCopyToRequest,
    RopCopyToSuccess, RopCreateAttachmentRequest, RopCreateMessageRequest, RopCreateMessageSuccess,
    RopDeleteAttachmentRequest, RopDeleteAttachmentResponse, RopDeleteMessagesRequest,
    RopDeleteMessagesResponse, RopDeletePropertiesRequest, RopErrorCode, RopErrorResponse,
    RopGetAttachmentTableRequest, RopGetAttachmentTableSuccess, RopGetPropertiesAllRequest,
    RopGetPropertiesSpecificRequest, RopGetStatusRequest, RopGetStreamSizeRequest,
    RopGetStreamSizeSuccess, RopGetValidAttachmentsRequest, RopGetValidAttachmentsSuccess,
    RopHeader4, RopId, RopLogonRequest, RopLogonSuccess, RopMoveCopyMessagesRequest,
    RopMoveCopyMessagesResponse, RopOpenAttachmentRequest, RopOpenAttachmentSuccess,
    RopOpenStreamRequest, RopOpenStreamSuccess, RopOpenTableRequest, RopPropertyWriteSuccess,
    RopQueryRowsRequest, RopReadStreamRequest, RopReadStreamSuccess,
    RopRegisterNotificationResponse, RopReleaseRequest, RopSaveChangesAttachmentRequest,
    RopSaveChangesAttachmentResponse, RopSaveChangesMessageRequest, RopSaveChangesMessageSuccess,
    RopSeekStreamRequest, RopSeekStreamSuccess, RopSetColumnsRequest, RopSetMessageReadFlagRequest,
    RopSetPropertiesRequest, RopSetStreamSizeRequest, RopSetStreamSizeResponse,
    RopSubmitMessageRequest, RopSubmitMessageResponse, RopTransportSendFailure,
    RopTransportSendRequest, RopTransportSendSuccess, RopWriteStreamRequest, RopWriteStreamSuccess,
};
use crate::mapi::session::{
    FolderKind, Handle, MapiNotificationSink, NotificationScope, SessionManager,
};
use crate::mapi::store;
use crate::mapi::transport::{MapiRequest, MapiRequestType, MapiResponse, ResponseCode, RpcKind};
use secrecy::ExposeSecret;

use crate::mapi::fxics::{IcsStreamBuilder, Marker, Tokenizer};
use crate::mapi::restrict::{CellForMatcher, SRestriction, restriction_referenced_tags};
use crate::mapi::rops::{
    RopCreateBookmarkRequest, RopCreateBookmarkResponse,
    RopFastTransferDestinationConfigureRequest, RopFastTransferDestinationPutBufferRequest,
    RopFastTransferDestinationPutBufferResponse, RopFastTransferSourceCopyFolderRequest,
    RopFastTransferSourceCopyMessagesRequest, RopFastTransferSourceCopyPropertiesRequest,
    RopFastTransferSourceCopyToRequest, RopFastTransferSourceGetBufferRequest,
    RopFastTransferSourceGetBufferSuccess, RopFastTransferSourceOpenResponse,
    RopFreeBookmarkRequest, RopFreeBookmarkResponse, RopNotifyResponse, RopPendingResponse,
    RopQueryPositionRequest, RopQueryPositionResponse, RopResetTableRequest, RopResetTableResponse,
    RopRestrictRequest, RopRestrictResponse, RopSeekRowBookmarkRequest, RopSeekRowBookmarkResponse,
    RopSeekRowFractionalRequest, RopSeekRowFractionalResponse, RopSeekRowRequest,
    RopSeekRowResponse, RopSortTableRequest, RopSortTableResponse, RopSynchronizationAckResponse,
    RopSynchronizationConfigureRequest, SortOrder,
};

/// Bundle of state the handler needs. Constructed once in `main.rs` (or a
/// test fixture) and shared across requests via `Arc`.
#[derive(Clone)]
pub struct MapiState {
    pub cfg: crate::config::Config,
    pub auth: std::sync::Arc<AuthVerifier>,
    pub sessions: SessionManager,
    /// Optional shared subscription manager. When present (production wires
    /// the same `Arc<SubscriptionManager>` the EWS path uses), the property-
    /// write arms publish `ItemModified` events so New Outlook's MAPI
    /// `NotificationWait` long-poll sees the change ŌĆö closing the EWS-only
    /// notification gap where a MAPI-triggered property write raised no event
    /// and the client aggressively re-polled (qodo #9, cubic #30, audit ┬¦2e).
    /// `None` in unit-test fixtures keeps them free of a live manager.
    pub subscription_manager: Option<std::sync::Arc<crate::notifications::SubscriptionManager>>,
    /// Optional gateway attachment store (the same `AttachmentManager` EWS /
    /// EAS already use). When present, the MAPI attachment write path
    /// (`RopSaveChangesAttachment` after `Blob/upload`, `RopDeleteAttachment`)
    /// persists the (email_id ŌåÆ blob_id/name/content_type/attach_num) record so
    /// `RopGetAttachmentTable` / `RopOpenAttachment` / `RopGetValidAttachments`
    /// round-trip a MAPI-composed attachment against Stalwart. `None` in
    /// unit-test fixtures keeps the write path free of an SQLite handle.
    pub attachment_manager: Option<std::sync::Arc<crate::attachment::AttachmentManager>>,
    /// Optional directory service backing the NSPI address-book surface
    /// (`/mapi/nspi`) — the same `Arc<dyn DirectoryLookup>` the EWS
    /// `ResolveNames` / `FindPeople` paths and the OAB download already use,
    /// backed by the Stalwart admin API when `admin_base` is configured (audit
    /// gap §2d). When `None` the NSPI dispatcher serves a *minimal GAL stub*
    /// containing only the authenticated caller's own mailbox entry so
    /// recipient resolution / "Check Names" for self still resolves; non-self
    /// resolutions return an empty result set, which is the documented
    /// behaviour of a directory-less Exchange-look-alike.
    pub directory: Option<std::sync::Arc<dyn crate::directory::DirectoryLookup>>,
    /// TTL cache of the directory-side GAL snapshot, shared across NSPI RPCs so
    /// a multi-RPC Outlook address-book handshake reuses one
    /// `search_blocking` resolution (the per-RPC amplification noted in PR #1845
    /// review). Allocated whenever a `directory` is wired; `None` in fixtures.
    pub gal_cache: Option<std::sync::Arc<crate::mapi::nspi::GalCache>>,
}

impl MapiState {
    pub fn new(cfg: Config, auth: std::sync::Arc<AuthVerifier>) -> Self {
        Self {
            cfg,
            auth,
            sessions: SessionManager::new(),
            subscription_manager: None,
            attachment_manager: None,
            directory: None,
            gal_cache: None,
        }
    }

    /// Production constructor: same as [`new`] but also wires the shared
    /// subscription manager so MAPI property writes publish notification
    /// events to the same feed EWS uses.
    pub fn with_subscription_manager(
        cfg: Config,
        auth: std::sync::Arc<AuthVerifier>,
        subscription_manager: std::sync::Arc<crate::notifications::SubscriptionManager>,
    ) -> Self {
        Self {
            cfg,
            auth,
            sessions: SessionManager::new(),
            subscription_manager: Some(subscription_manager),
            attachment_manager: None,
            directory: None,
            gal_cache: None,
        }
    }

    /// Wire the gateway attachment store onto the MAPI state. Called by the
    /// production `AppState` builder so `RopSaveChangesAttachment` /
    /// `RopDeleteAttachment` can persist MAPI-composed attachments against
    /// the same SQLite-backed manager EWS/EAS share.
    pub fn with_attachment_manager(
        mut self,
        mgr: std::sync::Arc<crate::attachment::AttachmentManager>,
    ) -> Self {
        self.attachment_manager = Some(mgr);
        self
    }

    /// Wire the operator-configured directory (Stalwart admin API) onto the
    /// MAPI state so the NSPI address-book dispatcher (`mapi::nspi`) can serve
    /// a real GAL for `Bind` / `QueryRows` / `DnToMinId` / `ResolveNames` /
    /// `GetMatches` rather than the caller-only minimal stub (audit gap §2d).
    pub fn with_directory(
        mut self,
        directory: std::sync::Arc<dyn crate::directory::DirectoryLookup>,
    ) -> Self {
        self.gal_cache = Some(std::sync::Arc::new(crate::mapi::nspi::GalCache::new()));
        self.directory = Some(directory);
        self
    }
}

/// Async entry: dispatch a parsed request to the matching handler.
pub async fn handle(req: MapiRequest, state: &MapiState) -> MapiResponse {
    if !state.cfg.mapi_enabled {
        return MapiResponse::error(ResponseCode::EndpointDisabled, req.request_id);
    }
    match req.kind {
        RpcKind::Mailbox(MapiRequestType::Connect) => handle_connect(req, state).await,
        RpcKind::Mailbox(MapiRequestType::Execute) => handle_execute(req, state).await,
        RpcKind::Mailbox(MapiRequestType::Disconnect) => handle_disconnect(req, state).await,
        RpcKind::Mailbox(MapiRequestType::NotificationWait) => {
            handle_notification_wait(req, state).await
        }
        RpcKind::Mailbox(MapiRequestType::Ping) => {
            MapiResponse::success(req.request_id, "PING", None, Vec::new())
        }
        RpcKind::AddressBook(rpc) => {
            // NSPI address-book dispatcher (MS-OXNSPI / MS-OXOABK served over
            // the MS-OXCMAPIHTTP §2.2.5 framing): Bind / Unbind / UpdateStat /
            // QueryRows / DnToMinId / ResolveNames / GetMatches / GetProps /
            // etc., backed by the operator-configured directory and
            // authenticated against the same `AuthVerifier` the mailbox path
            // uses (audit gap §2d).
            crate::mapi::nspi::handle_address_book(rpc, req, state).await
        }
    }
}

/// `Connect` RPC: the leading ROP must be `RopLogon`. On success we allocate
/// a session and emit the success envelope. We do NOT carry a transport-level
/// session cookie in Phase 0 ŌĆö Outlook will re-Connect if the server returns
/// ContextNotFound on the subsequent Execute; the session id is carried as
/// the X-ClientInfo extension once the gateway stores it there in Phase 1.
async fn handle_connect(req: MapiRequest, state: &MapiState) -> MapiResponse {
    let mut cur = Buf::new(&req.body);
    let rop = match RopLogonRequest::decode(&mut cur) {
        Ok(r) => r,
        Err(DecodeError::Insufficient) | Err(DecodeError::ExcessLength) => {
            return MapiResponse::error(ResponseCode::InvalidRequestBody, req.request_id);
        }
        Err(DecodeError::InvalidValue) | Err(DecodeError::InvalidUtf8) => {
            return MapiResponse::error(ResponseCode::InvalidRequestBody, req.request_id);
        }
        Err(DecodeError::Trailing) => {
            return MapiResponse::error(ResponseCode::InvalidRequestBody, req.request_id);
        }
    };
    // Phase 0 authenticates via Basic auth only. The password is supplied by
    // the router (in `main.rs`) from the request Authorization header.
    let password = req.password.as_deref();
    match logon_basic(&rop, password, &state.cfg, &state.auth, &state.sessions).await {
        LogonOutcome::Success {
            logon_id: _,
            envelope,
            session_id,
        } => {
            let mut body = Vec::new();
            encode_logon_success(&envelope, &mut body);
            // Per MS-OXCMAPIHTTP ┬¦3.2.5.1 / ┬¦4.1, the server MUST return the
            // session-context cookie that identifies the new Session Context
            // via Set-Cookie. Outlook stores it and echoes it back as a
            // `Cookie: MapiContext=<opaque>` header on every subsequent
            // Execute/Disconnect within that context. Without this, the
            // client cannot bind its ROPs to the just-created session, so every
            // Execute is dropped before reaching the dispatch layer. The
            // opaque value is the UUID server-assigned to the session.
            let cookie = format!("MapiContext={session_id}; Path=/mapi; HttpOnly");
            MapiResponse::success(
                req.request_id,
                "Connect",
                Some("ExchangeGateway/0.1".into()),
                body,
            )
            .with_session_cookie(cookie)
        }
        LogonOutcome::Failure { logon_id, error } => {
            let mut body = Vec::new();
            RopErrorResponse {
                rop_id: RopId::ROP_LOGON,
                output_handle_index: logon_id,
                return_value: error,
            }
            .encode(&mut body);
            MapiResponse::success(req.request_id, "Connect", None, body)
        }
    }
}

fn encode_logon_success(env: &RopLogonSuccess, out: &mut Vec<u8>) {
    env.encode(out);
}

/// The maximum server-side duration of a `NotificationWait` long-poll, per
/// MS-OXCMAPIHTTP §3.2.5.5 ("not sent until either the current server event
/// completes or the 5-minute maximum time limit expires"). Keeping this at
/// the spec ceiling maximises the window in which New Outlook receives a
/// real push instead of re-issuing the poll, without ever overrunning the
/// documented bound.
const NOTIFICATION_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Maximum number of `RopNotify` responses emitted in a single `Execute`
/// after a `NotificationWait` with `EventPending=1`, to bound the response
/// payload on a mailbox that has accumulated many changes. When more events
/// remain queued after this cap the gateway emits a `RopPending` so the
/// client issues another `Execute` to drain the rest (MS-OXCROPS §3.1.5.1.3).
const MAX_NOTIFY_PER_EXECUTE: usize = 32;

/// Build the MAPI `NotificationData` bytes (MS-OXCNOTIF §2.2.1.4.1.2) for a
/// single broadcast `NotificationEvent`, mapping the gateway feed's string
/// `folder_id`/`item_id` to the 64-bit MAPI row ids the wire format carries.
/// The `0x8000` message-event bit is always set here because every event the
/// gateway raises pertains to an item (mail/calendar/contact), never a folder
/// in the MAPI hierarchy sense — a future folder-create-event bridge would
/// clear it.
fn build_notification_data(event: &crate::mapi::session::NotificationEvent) -> Vec<u8> {
    use crate::mapi::session::NotificationEvent;
    use crate::mapi::session::{
        NT_NEW_MAIL, NT_OBJECT_COPIED, NT_OBJECT_CREATED, NT_OBJECT_DELETED, NT_OBJECT_MODIFIED,
        NT_OBJECT_MOVED,
    };
    use crate::mapi::store;
    // MAPI NotificationType bit (LS12) + the (folder, item) backend id strings
    // for the *destination* of the event, plus the optional *source* id strings
    // (only `ItemMoved`/`ItemCopied` carry Old* ids). The encoder treats the
    // Old* fields as MANDATORY for Moved/Copied per §2.2.1.4.1.2 — `None` ⇒ a
    // sentinel `0` is emitted, so a `RopNotify` for a move/copy is never
    // truncated even when the source ids are unknown.
    let (ty, folder_id_str, item_id_str, old_folder_id_str, old_item_id_str): (
        u16,
        &str,
        &str,
        Option<&str>,
        Option<&str>,
    ) = match event {
        NotificationEvent::NewMail {
            folder_id, item_id, ..
        } => (
            NT_NEW_MAIL,
            folder_id.as_str(),
            item_id.as_str(),
            None,
            None,
        ),
        NotificationEvent::ItemCreated {
            folder_id, item_id, ..
        } => (NT_OBJECT_CREATED, folder_id, item_id, None, None),
        NotificationEvent::ItemModified {
            folder_id, item_id, ..
        } => (NT_OBJECT_MODIFIED, folder_id, item_id, None, None),
        NotificationEvent::ItemDeleted {
            folder_id, item_id, ..
        } => (NT_OBJECT_DELETED, folder_id, item_id, None, None),
        NotificationEvent::ItemMoved {
            new_folder_id,
            new_item_id,
            old_folder_id,
            old_item_id,
            ..
        } => (
            NT_OBJECT_MOVED,
            new_folder_id,
            new_item_id,
            Some(old_folder_id.as_str()),
            Some(old_item_id.as_str()),
        ),
        NotificationEvent::ItemCopied {
            new_folder_id,
            new_item_id,
            old_folder_id,
            old_item_id,
            ..
        } => (
            NT_OBJECT_COPIED,
            new_folder_id,
            new_item_id,
            Some(old_folder_id.as_str()),
            Some(old_item_id.as_str()),
        ),
    };
    let flags = ty | 0x8000;
    let old_folder_id = old_folder_id_str.map(store::folder_id_from_backend);
    let old_message_id = old_item_id_str.map(store::message_id_from_jmap);
    // ParentFolderId / OldParentFolderId are search-folder (bit 0x4000) or
    // folder-event (bit 0x8000 clear) fields; the item-event feed sets 0x8000
    // and never 0x4000, so both stay `None` (omitted) here.
    let nd = crate::mapi::rops::NotificationData {
        notification_flags: flags,
        folder_id: store::folder_id_from_backend(folder_id_str),
        message_id: store::message_id_from_jmap(item_id_str),
        parent_folder_id: None,
        old_folder_id,
        old_message_id,
        old_parent_folder_id: None,
    };
    let mut out = Vec::with_capacity(34);
    nd.encode(&mut out);
    out
}

/// Resolve the session id carried by a `NotificationWait`/`Execute` request
/// (the `MapiContext` cookie, falling back to the X-ClientInfo extension UUID
/// emitted at RopLogon). Shared by `handle_notification_wait` and
/// `handle_execute` so both RPCs bind the same session.
fn resolve_session_id(req: &MapiRequest) -> Option<uuid::Uuid> {
    crate::mapi::transport::cookie_value(&req.cookies, "MapiContext")
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .or_else(|| req.client_info.as_deref().and_then(parse_client_info_uuid))
}

/// `NotificationWait` RPC (MS-OXCMAPIHTTP §2.2.4.4): a long-poll the client
/// issues after `RopRegisterNotification`. The server blocks until either an
/// event arrives on one of the session's registered notification sinks OR the
/// 5-minute maximum time limit expires, then returns the success response body
/// `StatusCode(4)=0 · ErrorCode(4)=0 · EventPending(4)=0|1 · AuxBufSize(4)=0`.
///
/// When `EventPending=1` the client issues an `Execute` (possibly with an empty
/// body) and the gateway's `handle_execute` drains the queued events into
/// `RopNotify` responses. This implementation reads from the per-session
/// notification registry (audit §2e): the broadcast feed the shared
/// `SubscriptionManager` publishes into — fed by the MAPI property-write arms
/// AND the EWS handlers — reaches the long-poll, so New Outlook's new-mail /
/// change push fires in real time and the client no longer spins re-polling.
///
/// When no `SubscriptionManager` is wired (unit-test fixtures), or the session
/// has no registered sinks, the long-poll still respects the 5-minute timeout
/// and returns `EventPending=0`, exactly closing the previous "immediate empty
/// success" that forced the client into an aggressive poll loop.
async fn handle_notification_wait(req: MapiRequest, state: &MapiState) -> MapiResponse {
    let session_id = resolve_session_id(&req);
    // Parse the request body per §2.2.4.4.1: Flags(4 LE) + AuxBufSize(4 LE) +
    // AuxBuf. We ignore the Flags (reserved; MUST be 0) and the auxiliary
    // buffer, but MUST consume the request body so a malformed-size field never
    // panics — bounded reads only.
    {
        let mut cur = Buf::new(&req.body);
        if cur.take_u32_le().is_ok()
            && let Ok(aux_size) = cur.take_u32_le()
        {
            // Cap the declared aux size at the remaining bytes (the transport
            // already bounded the whole body to MAX_MAPI_BODY_BYTES) and discard
            // up to that many bytes.
            let remaining = cur.remaining();
            let take = (aux_size as usize).min(remaining);
            for _ in 0..take {
                let _ = cur.take_u8();
            }
        }
    }

    let Some(session_id) = session_id else {
        // No session binding: transport-layer failure body (§2.2.4.4.3):
        // StatusCode != 0 + AuxBufSize = 0.
        let body = notification_wait_failure_body();
        return MapiResponse::success(req.request_id, "NotificationWait", None, body);
    };

    // If the session has expired (no snapshot found), the spec requires a
    // failure so the client re-Connects.
    if state.sessions.get(&session_id).is_none() {
        let body = notification_wait_failure_body();
        return MapiResponse::success(req.request_id, "NotificationWait", None, body);
    }

    let event_pending = notification_wait_poll(state, &session_id).await;
    let body = notification_wait_success_body(event_pending);
    MapiResponse::success(req.request_id, "NotificationWait", None, body)
}

/// The actual long-poll (MS-OXCMAPIHTTP §3.2.5.5): block up to
/// `NOTIFICATION_WAIT_MAX` for any of the session's registered notification
/// sinks to admit an event; return true iff at least one is queued for the
/// post-wait `Execute` to drain into `RopNotify` responses.
///
/// The implementation is non-destructive w.r.t. the `Execute` drain: a
/// `NotificationWait` only ever PUMPS each sink's broadcast receiver into the
/// sink's internal `pending` queue (and, for the blocking wait, blocks on the
/// receiver until one accepted event arrives). The queued events stay in the
/// sink's `pending` queue and are popped ONLY by `handle_execute`'s
/// `drain_for_execute` call — never here — so the post-wait `Execute` observes
/// exactly the events `NotificationWait` reported as pending (no double/drop).
async fn notification_wait_poll(state: &MapiState, session_id: &uuid::Uuid) -> bool {
    let registry = state.sessions.notifications();

    // Resolve the session's owner ONCE so the probe can filter events to this
    // mailbox without re-locking the session map per recv; an empty owner
    // (session vanished mid-turn) means there is nothing this turn can match.
    let owner = state
        .sessions
        .get(session_id)
        .map(|s| s.principal.email)
        .unwrap_or_default();
    let owner_norm = crate::mapi::session::canonicalize_owner(&owner);

    // No shared feed: the gateway in production ALWAYS wires a
    // SubscriptionManager (the same one the EWS path uses); the Only branch
    // that reaches here without one is a unit-test fixture that registers no
    // sinks. There is genuinely nothing to wait for — return EventPending=0
    // immediately rather than parking a Tokio worker for NOTIFICATION_WAIT_MAX
    // (which a fixture cannot afford and a real no-manager config would never
    // hit). The client re-polls at its own cadence.
    let Some(mgr) = state.subscription_manager.as_ref() else {
        return false;
    };

    // This session has no registered sinks (checked PER-SESSION, not across
    // every session — a chatty neighbour must not push an idle session into a
    // 5-minute probe). Nothing to wait for.
    if owner_norm.is_empty() || !registry.session_has_sinks(session_id) {
        return false;
    }

    // Subscribe the probe receiver BEFORE the initial pump so an event
    // published in the window between the pump and the subscription is still
    // observed by the probe (the broadcast fans out to every live receiver at
    // send time; a probe created after an event misses it). The per-sink
    // receivers are long-lived, so this race only concerns the probe.
    let mut probe = mgr.subscribe_raw();

    // 1) Pump every sink once and exit early if any already has a pending event.
    if registry.pump_and_has_pending(session_id) {
        return true;
    }

    // 2) Long-poll: wait on the probe for the next owner-matching event, then
    //    re-pump the per-sink receivers and re-check. A matching event on the
    //    shared feed is delivered to BOTH the probe and every per-sink receiver
    //    (broadcast fans out), so by the time the probe sees it the sinks' own
    //    receivers also hold it — the pump-and-check then reports
    //    `EventPending=1` without dropping the event (it stays in each sink's
    //    `pending` queue for the post-wait `Execute`). Non-matching (wrong
    //    owner / filtered type / filtered scope) keeps the turn alive for the
    //    remaining budget.
    let deadline = tokio::time::Instant::now() + NOTIFICATION_WAIT_MAX;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return registry.pump_and_has_pending(session_id);
        }
        match tokio::time::timeout(remaining, probe.recv()).await {
            Ok(Ok(ev)) if crate::mapi::session::canonicalize_owner(ev.owner()) == owner_norm => {
                if registry.pump_and_has_pending(session_id) {
                    return true;
                }
                // Probe saw an owner-matching event, but the session's sinks
                // filtered it out (type/folder scope). Keep waiting for the
                // remaining budget (this loop consumes one probe event per pass).
                continue;
            }
            Ok(Ok(_)) => continue, // wrong owner; keep waiting
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped))) => {
                // The probe fell behind the shared feed (a burst the slow probe
                // could not drain). Lagged is RECOVERABLE — the receiver
                // resynchronises on the next recv and the per-sink receivers
                // resync independently (each has its own lag window). Re-pump
                // in case an already-queued event was raised in the burst and
                // keep the turn alive; do NOT sleep the remaining budget.
                tracing::warn!(
                    skipped,
                    "MAPI NotificationWait probe lagged broadcast; resync"
                );
                if registry.pump_and_has_pending(session_id) {
                    return true;
                }
                continue;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                // Broadcast closed (the SubscriptionManager dropped). No more
                // events will ever arrive; honour the remaining budget so the
                // client's poll cadence is preserved, then return the final
                // pump (a queued event is still delivered even after closure).
                let rest = deadline.saturating_duration_since(tokio::time::Instant::now());
                if !rest.is_zero() {
                    tokio::time::sleep(rest).await;
                }
                return registry.pump_and_has_pending(session_id);
            }
            Err(_) => {
                // `remaining` elapsed before any event — return the final pump
                // (EventPending=0 heartbeat; the client re-issues the wait).
                return registry.pump_and_has_pending(session_id);
            }
        }
    }
}

/// Encode the `NotificationWait` success response body per
/// MS-OXCMAPIHTTP §2.2.4.4.2:
///   StatusCode(4 LE)=0 · ErrorCode(4 LE)=0 · EventPending(4 LE)=0|1 ·
///   AuxiliaryBufferSize(4 LE)=0
fn notification_wait_success_body(event_pending: bool) -> Vec<u8> {
    let mut body = Vec::with_capacity(16);
    body.extend_from_slice(&0u32.to_le_bytes()); // StatusCode = 0
    body.extend_from_slice(&0u32.to_le_bytes()); // ErrorCode = 0
    body.extend_from_slice(&(event_pending as u32).to_le_bytes()); // EventPending
    body.extend_from_slice(&0u32.to_le_bytes()); // AuxiliaryBufferSize = 0
    body
}

/// Encode the `NotificationWait` failure response body per
/// MS-OXCMAPIHTTP §2.2.4.4.3: `StatusCode(4 LE) != 0 · AuxiliaryBufferSize(4
/// LE)=0`. Used when the session cannot be resolved (the client re-Connects).
fn notification_wait_failure_body() -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&1u32.to_le_bytes()); // StatusCode != 0
    body.extend_from_slice(&0u32.to_le_bytes()); // AuxiliaryBufferSize = 0
    body
}

/// Drain the session's registered notification sinks into `out_body` as zero or
/// more `RopNotify` responses, followed by a `RopPending` when events remain
/// queued past the [`MAX_NOTIFY_PER_EXECUTE`] cap. Called at the top of every
/// `Execute` so the post-`NotificationWait` turn (where the client issued an
/// `EventPending=1`-triggered Execute, often empty) collects the events.
///
/// Each `RopNotify` carries the subscription's `OutputHandleIndex`
/// (zero-extended to the 4-byte `NotificationHandle`) so Outlook dispatches the
/// event to the right notification Server object, and the `LogonId` the client
/// associated with the registration. The `NotificationData` body is built by
/// [`build_notification_data`] per MS-OXCNOTIF §2.2.1.4.1.2.
fn emit_pending_notifications(out_body: &mut Vec<u8>, session_id: &uuid::Uuid, state: &MapiState) {
    let drained = state
        .sessions
        .notifications()
        .drain_for_execute(session_id, MAX_NOTIFY_PER_EXECUTE);
    if drained.is_empty() {
        return;
    }
    for (handle_index, logon_id, event) in drained {
        let notification_data = build_notification_data(&event);
        RopNotifyResponse {
            notification_handle: u32::from(handle_index),
            logon_id,
            notification_data,
        }
        .encode(out_body);
    }
    // If sinks still hold queued events, emit `RopPending` so the client issues
    // another `Execute` to drain them (MS-OXCROPS §3.1.5.1.3). The single
    // session here is session-index 0.
    if state.sessions.notifications().any_pending(session_id) {
        RopPendingResponse { session_index: 0 }.encode(out_body);
    }
}

/// `Execute` RPC: a buffer of one or more ROPs (MS-OXCROPS ┬¦3.2.5), each with
/// its own RopId + (LogonId) + handle indices. We decode them in order,
/// dispatch to a per-ROP handler that may bridge to the Stalwart backend,
/// and concatenate the per-ROP response bytes into a single Execute body.
///
/// Per MS-OXCMAPIHTTP ┬¦3.2.5.2 the Session Context is identified by the
/// `Cookie: MapiContext=<opaque>` header the client echoes after Connect.
/// We honour the cookie first and fall back to the (optional) X-ClientInfo
/// extension UUID emitted at RopLogon time, so the in-process unit tests
/// that drive this handler directly still resolve the session.
async fn handle_execute(req: MapiRequest, state: &MapiState) -> MapiResponse {
    // Session identity resolution is shared with `NotificationWait` via
    // [`resolve_session_id`] so both RPCs bind the same session (the
    // `MapiContext` cookie first, falling back to the X-ClientInfo extension
    // UUID emitted at RopLogon).
    let Some(session_id) = resolve_session_id(&req) else {
        // No session binding: return a transport-layer InvalidRequestBody
        // (code 12) — the client must RopLogon (Connect) first.
        return MapiResponse::error(ResponseCode::InvalidRequestBody, req.request_id);
    };

    let Some(snap) = state.sessions.get(&session_id) else {
        // Session expired/gone: a transport success wrapping a single
        // ROP-level `NotFound` so the client re-Connects. Parse the ROP
        // header bytes through a bounded `Buf` so an empty/truncated body
        // cannot panic on `[1..]`.
        let mut cur = Buf::new(&req.body);
        let leading = cur.take_u8().unwrap_or(0);
        let handle_index = cur.take_u8().unwrap_or(0);
        let mut body = Vec::new();
        RopErrorResponse {
            rop_id: RopId::from_u8(leading),
            output_handle_index: handle_index,
            return_value: RopErrorCode::NotFound,
        }
        .encode(&mut body);
        return MapiResponse::success(req.request_id, "Execute", None, body);
    };

    let username = snap.principal.email.clone();
    let password = req.password.clone();
    let logon_id = snap.logon_id.unwrap_or(0);

    // One cheap JmapClient per Execute ŌĆö the session cache inside it caches
    // the JMAP session per-username for 5 minutes, so the per-call cost is
    // just the cache lookup after the first request.
    let jmap = if !state.cfg.jmap_base.is_empty() {
        crate::jmap::JmapClient::new(&state.cfg.jmap_base).ok()
    } else {
        None
    };

    let mut cur = Buf::new(&req.body);
    let mut out_body = Vec::with_capacity(req.body.len() + 64);
    let password_secret = password.map(secrecy::SecretString::from);

    // If `NotificationWait` previously reported `EventPending=1`, the client
    // is now issuing an `Execute` (often with an empty body) to collect the
    // events. Drain the session's registered notification sinks up to
    // `MAX_NOTIFY_PER_EXECUTE` and emit one `RopNotify` per event BEFORE the
    // client's ROP-chain responses — exactly the order Outlook expects (audit
    // §2e closing the "MAPI NotificationWait never received anything" gap).
    // When more events remain queued after the cap, emit a `RopPending` so the
    // client issues another `Execute` to drain the rest (MS-OXCROPS §3.1.5.1.3).
    emit_pending_notifications(&mut out_body, &session_id, state);

    // ROP-chain loop: each iteration decodes one RopId plus the surrounding
    // header bytes per its spec, dispatches, and writes the response.
    while cur.remaining() > 0 {
        let start = cur.position();
        let rop_id = match cur.take_u8() {
            Ok(b) => RopId::from_u8(b),
            Err(_) => break,
        };
        let dispatch = execute_one_rop(
            rop_id,
            &mut cur,
            &mut out_body,
            &session_id,
            &state.sessions,
            &snap,
            jmap.as_ref(),
            &state.cfg,
            &username,
            password_secret.as_ref(),
            logon_id,
            state.subscription_manager.as_ref(),
            state.attachment_manager.as_ref(),
        )
        .await;
        if let Err(e) = dispatch {
            // An unrecoverable decode error: rewind is impossible (cursor
            // advanced past the bad ROP). Emit a single ROP-level error and
            // stop the chain ŌĆö the client will re-issue the unacked ROPs.
            tracing::warn!(?e, ?rop_id, pos = start, "Execute ROP decode failed");
            RopErrorResponse {
                rop_id,
                output_handle_index: 0,
                return_value: RopErrorCode::InvalidParameter,
            }
            .encode(&mut out_body);
            break;
        }
    }

    MapiResponse::success(req.request_id, "Execute", None, out_body)
}

/// Parse the leading UUID out of an `X-ClientInfo` value
/// (`{<guid>}:<routing>`). Returns `None` if the prefix is not a UUID.
fn parse_client_info_uuid(info: &str) -> Option<uuid::Uuid> {
    let head = info.split(':').next().unwrap_or(info);
    // Outlook braces the guid in `{...}`; strip them.
    let trimmed = head.trim_matches(|c| c == '{' || c == '}');
    uuid::Uuid::parse_str(trimmed).ok()
}

/// Outcome of a single ROP dispatch within an Execute chain.
type RopOutcome = Result<(), DecodeError>;

/// Discriminant for the shape of a handle resolved GetProperties* ŌĆö keeps
/// the dispatcher from confusing a `Handle::Message` and a `Handle::Folder`
/// that carry the same `FolderKind`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HandleShape {
    Message,
    Folder,
    Attachment,
    Neither,
}

/// Dispatch one ROP, writing its response bytes into `out`. The cursor
/// `cur` is positioned just past the RopId byte on entry.
#[allow(clippy::too_many_arguments)]
async fn execute_one_rop(
    rop_id: RopId,
    cur: &mut Buf<'_>,
    out: &mut Vec<u8>,
    session_id: &uuid::Uuid,
    sessions: &SessionManager,
    snap: &crate::mapi::session::SessionSnapshot,
    jmap: Option<&crate::jmap::JmapClient>,
    cfg: &Config,
    username: &str,
    password: Option<&secrecy::SecretString>,
    logon_id: u8,
    subscription_manager: Option<&std::sync::Arc<crate::notifications::SubscriptionManager>>,
    attachment_manager: Option<&std::sync::Arc<crate::attachment::AttachmentManager>>,
) -> RopOutcome {
    // Each ROP variant reads its own logon-id + handle indices per its spec
    // header shape, so the dispatch is per-variant rather than a uniform
    // header parse.
    match rop_id {
        RopId::ROP_RELEASE => {
            // ┬¦2.2.15.3.1: LogonId + InputHandleIndex
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _ = RopReleaseRequest::decode(cur)?;
            sessions.with_session_mut(session_id, |s| s.free_handle(input_handle_index));
            // If the freed index was a registered notification sink, drop it
            // too so its broadcast receiver is released (mirrors the spec's
            // RopRelease tearing down the notification Server object installed
            // by RopRegisterNotification; no-op for ordinary handle indices).
            sessions
                .notifications()
                .unregister(session_id, input_handle_index);
            crate::mapi::rops::RopReleaseResponse {
                input_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        RopId::ROP_OPEN_FOLDER => {
            // ┬¦2.2.4.1.1: 4-byte header then FolderId(8) + OpenModeFlags(1).
            // The dispatcher consumed the leading RopId byte before entering
            // this branch, so use `decode_after_ropid` to read only the
            // remaining LogonId┬ĘInput┬ĘOutput bytes (RopHeader4::decode would
            // re-consume a byte that is no longer present and silently
            // misinterpret the following payload as a header).
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = decode_open_folder_body(cur)?;
            let _ = req;
            // Resolve the folder backend id from the input handle (the root
            // or a leaf Folder handle populated by a hierarchy table).
            let backend_id = sessions
                .with_handle(session_id, h4.input_handle_index, |hnd| match hnd {
                    Handle::Folder { backend_id, .. } => backend_id.clone(),
                    _ => String::new(),
                })
                .unwrap_or_default();
            // Install the output handle pointing at the same folder.
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    h4.output_handle_index,
                    Handle::Folder {
                        backend_id: backend_id.clone(),
                        kind: folder_kind_for_backend(&backend_id, snap),
                    },
                );
            });
            crate::mapi::rops::RopOpenFolderSuccess {
                output_handle_index: h4.output_handle_index,
                return_value: RopErrorCode::Success,
                has_rules: 0,
                is_ghosted: 0,
            }
            .encode(out);
        }
        RopId::ROP_GET_HIERARCHY_TABLE | RopId::ROP_GET_CONTENTS_TABLE => {
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let _flags = RopOpenTableRequest::decode_body(cur)?;
            // Resolve the parent folder kind from the input handle.
            let (parent_backend, parent_kind) = sessions
                .with_handle(session_id, h4.input_handle_index, |h| match h {
                    Handle::Folder { backend_id, kind } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));

            let (rows, total, kind) = if rop_id == RopId::ROP_GET_HIERARCHY_TABLE {
                // Hierarchy table: enumerate JMAP mailboxes (and the
                // synthetic Calendar/Contacts folders) for the principal.
                let mailboxes = if let Some(jc) = jmap {
                    if let Some(pw) = password {
                        jc.query_mailboxes(username, pw).await.ok()
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut rows = mailboxes
                    .map(|ml| ml.mailboxes)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mbx| {
                        let bid = mbx.id.clone().unwrap_or_default();
                        let row_id = store::folder_id_from_backend(&bid);
                        let cells: Vec<crate::mapi::data::PropertyValue> = Vec::new();
                        // Synthesize Calendar/Contacts rows (JMAP has no
                        // mailbox for those ŌĆö CalDAV/CardDAV own them) so
                        // the hierarchy table exposes them as folders. For
                        // mail mailboxes we stash the JmapMailbox for lazy
                        // cell materialisation.
                        let source: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                            std::sync::Arc::new(mbx.clone());
                        crate::mapi::session::TableRow {
                            row_id,
                            cells,
                            source: Some(source),
                        }
                    })
                    .collect::<Vec<_>>();
                // Append the gateway-owned virtual Calendar/Contacts folders.
                // They carry a synthetic `JmapMailbox` (role
                // `__calendar__`/`__contacts__`) so `mailbox_to_cells`
                // renders the correct `PR_CONTAINER_CLASS` (IPF.Appointment
                // / IPF.Contact) and the contents-table-open step resolves
                // the folder kind — JMAP models no Mailbox for these.
                rows.push(synth_folder_row(
                    store::CALENDAR_BACKEND_ID,
                    "Calendar",
                    store::CALENDAR_BACKEND_ID,
                ));
                rows.push(synth_folder_row(
                    store::CONTACTS_BACKEND_ID,
                    "Contacts",
                    store::CONTACTS_BACKEND_ID,
                ));
                let total = rows.len() as u64;
                (rows, total, FolderKind::Mail)
            } else {
                // Contents table: enumerate the messages in the parent folder.
                // Calendar/Contacts are CalDAV/CardDAV-backed, not JMAP, so
                // serve them even when JMAP is unconfigured (the synthetic
                // folder ids bypass the JMAP mailbox set entirely).
                let rows = if let Some(pw) = password {
                    match parent_kind {
                        FolderKind::Calendar => {
                            fetch_calendar_rows(cfg, username, pw, &parent_backend).await
                        }
                        FolderKind::Contacts => {
                            fetch_contact_rows(cfg, username, pw, &parent_backend).await
                        }
                        _ => {
                            if let Some(jc) = jmap {
                                fetch_email_rows(cfg, jc, username, pw, &parent_backend).await
                            } else {
                                Vec::new()
                            }
                        }
                    }
                } else {
                    Vec::new()
                };
                let total = rows.len() as u64;
                (rows, total, parent_kind)
            };
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    h4.output_handle_index,
                    Handle::Table {
                        kind,
                        parent_handle: h4.input_handle_index as i16,
                        parent_backend_id: parent_backend.clone(),
                        column_set: Vec::new(),
                        rows,
                        cursor: 0,
                        total,
                        restriction: crate::mapi::restrict::SRestriction::default(),
                        sort_orders: Vec::new(),
                        next_bookmark: 0,
                    },
                );
            });
            crate::mapi::rops::RopOpenTableSuccess {
                output_handle_index: h4.output_handle_index,
                return_value: RopErrorCode::Success,
                row_count: u32::try_from(total.min(u32::MAX as u64)).unwrap_or(0),
            }
            .encode(out, rop_id);
        }
        RopId::ROP_SET_COLUMNS => {
            // ┬¦2.2.5.1.1: LogonId + InputHandleIndex + SetColumnFlags(1)
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSetColumnsRequest::decode(cur)?;
            sessions.with_session_mut(session_id, |s| {
                if let Some(Handle::Table {
                    column_set, rows, ..
                }) = s.handle_mut(input_handle_index)
                {
                    *column_set = req.property_tags.clone();
                    // Invalidate the per-row cached cells so the next QueryRows
                    // re-materialises against the new column set. The cached
                    // `source` (if not yet dropped) stays put; if it was
                    // already consumed the rows will fall back to typed NULLs.
                    for r in rows.iter_mut() {
                        r.cells.clear();
                    }
                }
            });
            crate::mapi::rops::RopSetColumnsSuccess {
                input_handle_index,
                return_value: RopErrorCode::Success,
                table_status: 0,
            }
            .encode(out);
        }
        RopId::ROP_QUERY_ROWS => {
            // ┬¦2.2.5.4.1: LogonId + InputHandleIndex + QueryRowsFlags(1)
            // + ForwardRead(1) + RowCount(2 LE)
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopQueryRowsRequest::decode(cur)?;
            // Re-materialise cells for the current column set if the table
            // rows are still empty (Stalwarts mailboxes were fetched as bare
            // row ids; the property cells come from Email/get on demand).
            let (row_data, served, origin) = sessions
                .with_session_mut(session_id, |s| {
                    let Some(Handle::Table {
                        rows,
                        cursor,
                        column_set,
                        kind,
                        parent_backend_id,
                        restriction,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    else {
                        return (Vec::new(), 0u16, 0u8);
                    };
                    let cs = column_set.clone();
                    let pk = *kind;
                    let mailbox_id = parent_backend_id.clone();
                    let rst = restriction.clone();
                    // Build the filtered view: the indices of rows the active
                    // restriction admits, using the SAME builder RopRestrict
                    // uses to derive `total`, so the count QueryPosition
                    // reports equals the rows QueryRows would serve. The
                    // builder also materialises cells over the UNION of the
                    // column set and the restriction's referenced tags, so a
                    // Restrict issued before SetColumns still resolves.
                    let filtered = filtered_indices(rows, &cs, &rst, pk, &mailbox_id);
                    let want = usize::from(req.row_count)
                        .min(filtered.len().saturating_sub((*cursor).min(filtered.len())));
                    let mut buf = Vec::new();
                    let served = u16::try_from(want).unwrap_or(0);
                    for &ix in filtered.iter().skip(*cursor).take(want) {
                        let r = &mut rows[ix];
                        // Lazily materialise cells for the current column
                        // set from the cached backend object carried on the
                        // row, then drop the cached source to bound memory.
                        if r.cells.is_empty()
                            && !cs.is_empty()
                            && let Some(src) = r.source.take()
                        {
                            if let Some(e) = src.downcast_ref::<crate::jmap::JmapEmail>() {
                                r.cells = store::email_to_cells(e, &cs, pk, mailbox_id.as_str());
                            } else if let Some(m) = src.downcast_ref::<crate::jmap::JmapMailbox>() {
                                r.cells = store::mailbox_to_cells(m, &cs);
                            } else if let Some(a) =
                                src.downcast_ref::<crate::jmap::JmapAttachment>()
                            {
                                // Attachment-table row: row_id is the
                                // PR_ATTACH_NUM (a u32 reinterpreted as u64).
                                let num = u32::try_from(r.row_id).unwrap_or(0);
                                r.cells = store::attachment_to_cells(a, num, &cs);
                            } else if let Some(c) =
                                src.downcast_ref::<crate::calendar::CalendarItem>()
                            {
                                r.cells = crate::mapi::converters::calendar_to_cells(
                                    c,
                                    &cs,
                                    mailbox_id.as_str(),
                                );
                            } else if let Some(v) = src.downcast_ref::<String>() {
                                r.cells = crate::mapi::converters::contact_to_cells(
                                    v,
                                    &cs,
                                    mailbox_id.as_str(),
                                );
                            }
                        }
                        // Emit a StandardPropertyRow (flag=0): one flag byte
                        // + the per-column PropertyValue bytes (no tag prefix
                        // ŌĆö the column order echoes the SetColumns request).
                        buf.push(0u8);
                        let cells = r.cells.clone();
                        for (tag, cell) in cs.iter().zip(cells) {
                            encode_cell_for_row(&mut buf, tag, cell, r);
                        }
                    }
                    *cursor += want;
                    let origin = if req.forward_read != 0 { 0u8 } else { 1 };
                    (buf, served, origin)
                })
                .unwrap_or_default();
            crate::mapi::rops::RopQueryRowsSuccess {
                input_handle_index,
                return_value: RopErrorCode::Success,
                origin,
                row_count: served,
                row_data,
            }
            .encode(out);
        }
        RopId::ROP_GET_STATUS => {
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _ = RopGetStatusRequest::decode(cur)?;
            crate::mapi::rops::RopGetStatusSuccess {
                input_handle_index,
                return_value: RopErrorCode::Success,
                table_status: 0,
            }
            .encode(out);
        }
        RopId::ROP_GET_PROPERTIES_SPECIFIC => {
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopGetPropertiesSpecificRequest::decode(cur)?;
            // Resolve the live JMAP object from the input handle and run the
            // store.rs converter for the requested property tags. For a
            // Message handle this fetches the full JmapEmail via Email/get;
            // for a Folder handle it maps the JmapMailbox; for a Table handle
            // it uses the materialised cell row at the cursor. A missing
            // object or a non-message/folder handle falls back to typed NULLs.
            // Resolve the live backend object from the input handle. We track
            // the handle *shape* (Message vs Folder) because the same
            // FolderKind discriminant can mean either over the live session.
            // One handle snapshot carries everything GetProperties needs:
            // shape/kind/backend/mailbox for the message/folder paths, plus
            // `attach_num` and the cached attachment metadata (name /
            // content_type / size, captured at `RopOpenAttachment`) for the
            // attachment path. The latter lets an Attachment handle's cells be
            // materialised from the cached metadata with NO JMAP round-trip
            // (Outlook issues GetPropertiesSpecific repeatedly per
            // attachment), falling back to Email/get only when the cached
            // metadata is absent.
            let (handle_shape, kind, backend_id, mailbox_id, attach_num, cached_name) = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        kind,
                        ..
                    } => (
                        HandleShape::Message,
                        *kind,
                        backend_id.clone(),
                        mailbox_id.clone(),
                        0,
                        None,
                    ),
                    Handle::Folder { backend_id, kind } => (
                        HandleShape::Folder,
                        *kind,
                        backend_id.clone(),
                        String::new(),
                        0,
                        None,
                    ),
                    Handle::Attachment {
                        email_id,
                        kind,
                        attach_num,
                        name,
                        content_type,
                        size,
                        ..
                    } => (
                        HandleShape::Attachment,
                        *kind,
                        email_id.clone(),
                        String::new(),
                        *attach_num,
                        Some(crate::jmap::JmapAttachment {
                            id: None,
                            blob_id: None,
                            size: *size,
                            content_type: Some(content_type.clone()),
                            name: Some(name.clone()),
                        }),
                    ),
                    _ => (
                        HandleShape::Neither,
                        FolderKind::Root,
                        String::new(),
                        String::new(),
                        0,
                        None,
                    ),
                })
                .unwrap_or((
                    HandleShape::Neither,
                    FolderKind::Root,
                    String::new(),
                    String::new(),
                    0,
                    None,
                ));
            let cells = match (handle_shape, jmap, password) {
                // Mail message handle -> Email/get -> email_to_cells (body,
                // sender, subject, flags, entry-id, etc.).
                (HandleShape::Message, Some(jc), Some(pw))
                    if kind == FolderKind::Mail && !backend_id.is_empty() =>
                {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        Vec::new()
                    } else {
                        match jc.get_email(&account_id, &backend_id, username, pw).await {
                            Ok(Some(e)) => {
                                store::email_to_cells(&e, &req.property_tags, kind, &mailbox_id)
                            }
                            _ => Vec::new(),
                        }
                    }
                }
                // Mail folder handle -> Mailbox/query -> mailbox_to_cells
                // (DisplayName, ParentFolderId, ContentCount, ...).
                (HandleShape::Folder, Some(jc), Some(pw))
                    if kind == FolderKind::Mail && !backend_id.is_empty() =>
                {
                    match jc.query_mailboxes(username, pw).await {
                        Ok(ml) => ml
                            .mailboxes
                            .into_iter()
                            .find(|m| m.id.as_deref() == Some(backend_id.as_str()))
                            .map(|m| store::mailbox_to_cells(&m, &req.property_tags))
                            .unwrap_or_default(),
                        Err(_) => Vec::new(),
                    }
                }
                // Mail attachment handle: serve the cells from the metadata
                // cached on the handle at `RopOpenAttachment` time (name /
                // content_type / size) with NO JMAP round-trip ŌĆö Outlook
                // issues GetPropertiesSpecific repeatedly per attachment. Only
                // fall back to Email/get when the handle carries no cached
                // metadata (a degenerate handle) so we still resolve the
                // indexed `attachments[]` entry; `Ok(None)` and JMAP errors
                // surface typed NULLs rather than a fabricated "no metadata".
                (HandleShape::Attachment, Some(jc), Some(pw))
                    if kind == FolderKind::Mail && !backend_id.is_empty() =>
                {
                    if let Some(att) = &cached_name {
                        store::attachment_to_cells(att, attach_num, &req.property_tags)
                    } else {
                        let account_id = jc
                            .get_account_id(username, pw)
                            .await
                            .ok()
                            .unwrap_or_default();
                        if account_id.is_empty() {
                            store::typed_null_cells(&req.property_tags)
                        } else {
                            match jc.get_email(&account_id, &backend_id, username, pw).await {
                                Ok(Some(e)) => match store::email_attachment_by_num(&e, attach_num)
                                {
                                    Some(att) => store::attachment_to_cells(
                                        att,
                                        attach_num,
                                        &req.property_tags,
                                    ),
                                    None => store::typed_null_cells(&req.property_tags),
                                },
                                Ok(None) => store::typed_null_cells(&req.property_tags),
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "JMAP get_email (GetPropertiesSpecific attachment) failed"
                                    );
                                    store::typed_null_cells(&req.property_tags)
                                }
                            }
                        }
                    }
                }
                // Calendar message handle -> CalDAV window fetch -> match by
                // row id -> calendar_to_cells (IPM.Appointment). Outlook opens
                // an appointment via the contents-table row first (cached
                // CalendarItem); a Message handle opened without that cache
                // re-queries the CalDAV window and matches by FNV-1a row id
                // (the stable mapping of the iCalendar UID).
                (HandleShape::Message, Some(_jc), Some(pw))
                    if kind == FolderKind::Calendar && !backend_id.is_empty() =>
                {
                    let target = u64::from_str_radix(&backend_id, 16)
                        .unwrap_or_else(|_| store::folder_id_from_backend(&backend_id));
                    fetch_calendar_rows(cfg, username, pw, store::CALENDAR_BACKEND_ID)
                        .await
                        .into_iter()
                        .find_map(|r| {
                            if r.row_id == target
                                && let Some(src) = r.source
                                && let Some(c) = src.downcast_ref::<crate::calendar::CalendarItem>()
                            {
                                Some(crate::mapi::converters::calendar_to_cells(
                                    c,
                                    &req.property_tags,
                                    store::CALENDAR_BACKEND_ID,
                                ))
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| store::typed_null_cells(&req.property_tags))
                }
                // Calendar folder handle -> synth JmapMailbox -> mailbox_to_cells
                // (PR_CONTAINER_CLASS = IPF.Appointment, PR_DISPLAY_NAME).
                (HandleShape::Folder, _, _)
                    if kind == FolderKind::Calendar && !backend_id.is_empty() =>
                {
                    let mbx = crate::jmap::JmapMailbox {
                        id: Some(backend_id.clone()),
                        name: Some("Calendar".to_string()),
                        parent_id: Some("ROOT".to_string()),
                        role: Some(store::CALENDAR_BACKEND_ID.to_string()),
                        sort_order: None,
                        total_emails: None,
                        unread_emails: None,
                        total_threads: None,
                        unread_threads: None,
                        is_subscribed: None,
                    };
                    store::mailbox_to_cells(&mbx, &req.property_tags)
                }
                // Contacts message handle -> CardDAV list_contacts ->
                // contact_to_cells (IPM.Contact). Match by FNV-1a row id
                // (derived from the CardDAV href).
                (HandleShape::Message, Some(_jc), Some(pw))
                    if kind == FolderKind::Contacts && !backend_id.is_empty() =>
                {
                    let target = u64::from_str_radix(&backend_id, 16)
                        .unwrap_or_else(|_| store::folder_id_from_backend(&backend_id));
                    fetch_contact_rows(cfg, username, pw, store::CONTACTS_BACKEND_ID)
                        .await
                        .into_iter()
                        .find_map(|r| {
                            if r.row_id == target
                                && let Some(src) = r.source
                                && let Some(v) = src.downcast_ref::<String>()
                            {
                                Some(crate::mapi::converters::contact_to_cells(
                                    v,
                                    &req.property_tags,
                                    store::CONTACTS_BACKEND_ID,
                                ))
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| store::typed_null_cells(&req.property_tags))
                }
                // Contacts folder handle -> synth JmapMailbox -> mailbox_to_cells
                // (PR_CONTAINER_CLASS = IPF.Contact, PR_DISPLAY_NAME).
                (HandleShape::Folder, _, _)
                    if kind == FolderKind::Contacts && !backend_id.is_empty() =>
                {
                    let mbx = crate::jmap::JmapMailbox {
                        id: Some(backend_id.clone()),
                        name: Some("Contacts".to_string()),
                        parent_id: Some("ROOT".to_string()),
                        role: Some(store::CONTACTS_BACKEND_ID.to_string()),
                        sort_order: None,
                        total_emails: None,
                        unread_emails: None,
                        total_threads: None,
                        unread_threads: None,
                        is_subscribed: None,
                    };
                    store::mailbox_to_cells(&mbx, &req.property_tags)
                }
                // Calendar / Contacts / Root / missing backend: typed NULLs
                // (their backend wiring is Phase-3).
                _ => store::typed_null_cells(&req.property_tags),
            };
            let row = crate::mapi::session::TableRow {
                row_id: store::message_id_from_jmap(&backend_id),
                cells: Vec::new(),
                source: None,
            };
            let mut buf = Vec::new();
            // store::email_to_cells / mailbox_to_cells emit exactly one value
            // per requested tag (typed Null for unknown/unsupported columns),
            // so zip covers the whole column set without need for backfill.
            for (tag, cell) in req.property_tags.iter().zip(cells) {
                encode_cell_for_row(&mut buf, tag, cell, &row);
            }
            crate::mapi::rops::RopGetPropertiesSuccess {
                rop_id,
                input_handle_index,
                return_value: RopErrorCode::Success,
                row_data: buf,
            }
            .encode(out);
        }
        RopId::ROP_GET_PROPERTIES_ALL => {
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _ = RopGetPropertiesAllRequest::decode(cur)?;
            let row_data = Vec::new();
            crate::mapi::rops::RopGetPropertiesSuccess {
                rop_id,
                input_handle_index,
                return_value: RopErrorCode::Success,
                row_data,
            }
            .encode(out);
        }
        RopId::ROP_GET_ATTACHMENT_TABLE => {
            // ┬¦2.2.6.17.1: RopId ┬Ę LogonId ┬Ę InputHandleIndex ┬Ę
            // OutputHandleIndex ┬Ę TableFlags(1) ŌĆö a 4-byte RopHeader4 body
            // followed by TableFlags. The input handle is the Message whose
            // attachments we enumerate; the output handle is the new Table.
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = RopGetAttachmentTableRequest::decode_body(
                cur,
                h4.input_handle_index,
                h4.output_handle_index,
            )?;
            // Resolve the owning message (mail only ŌĆö calendar/contact
            // attachments are not enumerated through MAPI in this phase).
            let (email_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id, kind, ..
                    } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            if kind != FolderKind::Mail || email_id.is_empty() {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            // Fetch the live JmapEmail so the attachment table enumerates the
            // real `attachments[]`. Distinguish the three `Email/get` outcomes:
            // `Ok(Some)` builds the rows; `Ok(None)` (email genuinely absent)
            // installs an empty table (the client sees "no attachments");
            // `Err` (transient backend failure) returns a typed `DiskError`
            // rather than a fabricated empty table that would make Outlook
            // believe a message with attachments has none ŌĆö matching the
            // `RopOpenAttachment` arm's error handling.
            enum AttachFetch {
                Rows(Vec<crate::mapi::session::TableRow>),
                Empty,
                Failed,
            }
            let fetched = if let (Some(jc), Some(pw)) = (jmap, password) {
                let account_id = jc
                    .get_account_id(username, pw)
                    .await
                    .ok()
                    .unwrap_or_default();
                if account_id.is_empty() {
                    AttachFetch::Empty
                } else {
                    match jc.get_email(&account_id, &email_id, username, pw).await {
                        Ok(Some(e)) => AttachFetch::Rows(
                            store::email_attach_nums(&e)
                                .into_iter()
                                .map(|num| {
                                    let att = store::email_attachment_by_num(&e, num).cloned();
                                    let row_id = u64::from(num);
                                    // Stash the JmapAttachment as the row source so
                                    // QueryRows lazily materialises
                                    // attachment_to_cells once SetColumns fixes the
                                    // column set.
                                    let source = att.map(|a| {
                                        std::sync::Arc::new(a)
                                            as std::sync::Arc<dyn std::any::Any + Send + Sync>
                                    });
                                    crate::mapi::session::TableRow {
                                        row_id,
                                        cells: Vec::new(),
                                        source,
                                    }
                                })
                                .collect::<Vec<_>>(),
                        ),
                        Ok(None) => AttachFetch::Empty,
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "JMAP get_email (GetAttachmentTable) failed"
                            );
                            AttachFetch::Failed
                        }
                    }
                }
            } else {
                AttachFetch::Empty
            };
            if matches!(fetched, AttachFetch::Failed) {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::DiskError,
                }
                .encode(out);
                return Ok(());
            }
            let rows = match fetched {
                AttachFetch::Rows(r) => r,
                AttachFetch::Empty => Vec::new(),
                AttachFetch::Failed => unreachable!("returned above"),
            };
            let total = rows.len() as u64;
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    req.output_handle_index,
                    Handle::Table {
                        kind: FolderKind::Mail,
                        parent_handle: req.input_handle_index as i16,
                        parent_backend_id: email_id.clone(),
                        column_set: Vec::new(),
                        rows,
                        cursor: 0,
                        total,
                        restriction: crate::mapi::restrict::SRestriction::default(),
                        sort_orders: Vec::new(),
                        next_bookmark: 0,
                    },
                );
            });
            // Spec ┬¦2.2.6.17.2: the response is the success envelope only
            // (RopId ┬Ę OutputHandleIndex ┬Ę ReturnValue). The row count is
            // delivered via the subsequent RopQueryRows against the table, not
            // in this envelope, so we do not append one here.
            RopGetAttachmentTableSuccess {
                output_handle_index: req.output_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        RopId::ROP_OPEN_ATTACHMENT => {
            // ┬¦2.2.6.12.1: RopId ┬Ę LogonId ┬Ę InputHandleIndex ┬Ę
            // OutputHandleIndex ┬Ę OpenAttachmentFlags(1) ┬Ę AttachmentID(4 LE).
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = RopOpenAttachmentRequest::decode_body(
                cur,
                h4.input_handle_index,
                h4.output_handle_index,
            )?;
            // Resolve owning message + its JMAP attachment at attach_num.
            let (email_id, mailbox_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        kind,
                        ..
                    } => (backend_id.clone(), mailbox_id.clone(), *kind),
                    _ => (String::new(), String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), String::new(), FolderKind::Root));
            if kind != FolderKind::Mail || email_id.is_empty() {
                RopOpenAttachmentSuccess {
                    output_handle_index: h4.output_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            // Verify the requested PR_ATTACH_NUM exists on the message; on
            // success capture the JMAP blob id + declared size so a subsequent
            // `RopOpenStream(PR_ATTACH_DATA_BIN)` resolves the specific blob
            // (and the >1-attachment case that previously failed closed now
            // works through this handle) and reports a real `stream_size` /
            // enforces `max_attachment_bytes` without an extra round-trip.
            let (return_value, blob_id, name, content_type, size) = match (jmap, password) {
                (Some(jc), Some(pw)) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        (
                            RopErrorCode::NotFound,
                            String::new(),
                            String::new(),
                            String::new(),
                            None,
                        )
                    } else {
                        match jc.get_email(&account_id, &email_id, username, pw).await {
                            Ok(Some(e)) => {
                                match store::email_attachment_by_num(&e, req.attachment_id) {
                                    Some(att) => (
                                        RopErrorCode::Success,
                                        att.blob_id.clone().unwrap_or_default(),
                                        att.name.clone().unwrap_or_default(),
                                        att.content_type.clone().unwrap_or_else(|| {
                                            "application/octet-stream".to_string()
                                        }),
                                        att.size,
                                    ),
                                    None => (
                                        RopErrorCode::NotFound,
                                        String::new(),
                                        String::new(),
                                        String::new(),
                                        None,
                                    ),
                                }
                            }
                            Ok(None) => (
                                RopErrorCode::NotFound,
                                String::new(),
                                String::new(),
                                String::new(),
                                None,
                            ),
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP get_email (OpenAttachment) failed");
                                (
                                    RopErrorCode::DiskError,
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                    None,
                                )
                            }
                        }
                    }
                }
                _ => (
                    RopErrorCode::AccessDenied,
                    String::new(),
                    String::new(),
                    String::new(),
                    None,
                ),
            };
            if return_value != RopErrorCode::Success {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: h4.output_handle_index,
                    return_value,
                }
                .encode(out);
                return Ok(());
            }
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    h4.output_handle_index,
                    Handle::Attachment {
                        email_id: email_id.clone(),
                        mailbox_id: mailbox_id.clone(),
                        kind: FolderKind::Mail,
                        attach_num: req.attachment_id,
                        blob_id,
                        name,
                        content_type,
                        size,
                        is_new: false,
                    },
                );
            });
            RopOpenAttachmentSuccess {
                output_handle_index: h4.output_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        RopId::ROP_GET_VALID_ATTACHMENTS => {
            // ┬¦2.2.6.18.1: RopId ┬Ę LogonId ┬Ę InputHandleIndex.
            let _logon = cur.take_u8()?;
            let req = RopGetValidAttachmentsRequest::decode_after_ropid(cur)?;
            // The input handle MUST be a mail Message; enumerate its
            // `PR_ATTACH_NUM` ids from the live JMAP email.
            let (email_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id, kind, ..
                    } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            if kind != FolderKind::Mail || email_id.is_empty() {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.input_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            let ids = if let (Some(jc), Some(pw)) = (jmap, password) {
                let account_id = jc
                    .get_account_id(username, pw)
                    .await
                    .ok()
                    .unwrap_or_default();
                if account_id.is_empty() {
                    Vec::new()
                } else {
                    match jc.get_email(&account_id, &email_id, username, pw).await {
                        Ok(Some(e)) => store::email_attach_nums(&e),
                        Ok(None) => Vec::new(),
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "JMAP get_email (GetValidAttachments) failed"
                            );
                            RopErrorResponse {
                                rop_id,
                                output_handle_index: req.input_handle_index,
                                return_value: RopErrorCode::DiskError,
                            }
                            .encode(out);
                            return Ok(());
                        }
                    }
                }
            } else {
                Vec::new()
            };
            RopGetValidAttachmentsSuccess {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::Success,
                attachment_ids: ids,
            }
            .encode(out);
        }
        RopId::ROP_SET_MESSAGE_READ_FLAG => {
            // Per MS-OXCROPS ┬¦2.2.6.11.1 the post-RopId bytes are
            // LogonId ┬Ę ResponseHandleIndex ┬Ę InputHandleIndex ┬Ę ReadFlags.
            // Consume all three header bytes here: the InputHandleIndex is
            // the Message handle, ResponseHandleIndex is what we echo back in
            // the response, ReadFlags is the body.
            let _logon = cur.take_u8()?;
            let response_handle_index = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSetMessageReadFlagRequest::decode(cur)?;
            // ReadFlags (MS-OXCMSG ┬¦2.2.3.11.1) is a BITMASK, not an
            // equality. rfClearReadFlag (0x04) clears the mfRead bit,
            // rfGenerateReceiptOnly (0x10) leaves mfRead unchanged, anything
            // else (rfDefault 0x00 / rfSuppressReceipt 0x01) sets mfRead.
            // Treat absent receipt implementation as a no-op bit-wise; we
            // only persist `$seen`.
            const RF_CLEAR_READ_FLAG: u8 = 0x04;
            const RF_GENERATE_RECEIPT_ONLY: u8 = 0x10;
            let backend_id = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message { backend_id, .. } => Some(backend_id.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            let want_read = !(req.read_flag & RF_CLEAR_READ_FLAG != 0
                || req.read_flag & RF_GENERATE_RECEIPT_ONLY != 0);
            let backend_owned = if backend_id.is_empty() {
                None
            } else {
                Some(backend_id.as_str())
            };
            // Distinguishing ROP failures from transport success: when we
            // cannot apply the patch (no JMAP config, no creds, no account
            // id, missing message handle), return ROP-level `DiskError`
            // rather than silently reporting `Success` (cubic #66944/cr #5227).
            let outcome: RopErrorCode = match (jmap, password, backend_owned) {
                (Some(jc), Some(pw), Some(id)) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else {
                        let key = "keywords/$seen".to_string();
                        let update = if want_read {
                            serde_json::json!({ id: { key.clone(): true } })
                        } else {
                            serde_json::json!({ id: { key.clone(): serde_json::Value::Null } })
                        };
                        match jc.update_email(&account_id, &update, username, pw).await {
                            Ok(()) => RopErrorCode::Success,
                            Err(_) => RopErrorCode::DiskError,
                        }
                    }
                }
                (None, _, _) => RopErrorCode::NotFound, // no JMAP backend configured
                (_, None, _) => RopErrorCode::AccessDenied, // no credentials
                (_, _, None) => RopErrorCode::NotFound, // message handle not bound
            };
            // ResponseHandleIndex echoes the request's ResponseHandleIndex
            // (MS-OXCROPS ┬¦2.2.6.11.2), NOT the InputHandleIndex used to
            // identify the message.
            RopErrorResponse {
                rop_id,
                output_handle_index: response_handle_index,
                return_value: outcome,
            }
            .encode(out);
        }
        // ---- Mail write path (audit ┬¦2a): compose / save / send / delete / move-
        // All six arms bridge to JMAP `Email/set` (create/update), `Email/destroy`,
        // `Email/set` mailboxIds patch (move) / `Email/set` copyFrom (copy), and
        // `EmailSubmission/set` (send). A missing JMAP backend, missing creds, or
        // an unbound handle yields a typed ROP-level error (NotFound /
        // AccessDenied / DiskError) so the client can react instead of a silent
        // Success-with-empty-state.
        RopId::ROP_CREATE_MESSAGE => {
            // ┬¦2.2.6.2.1: LogonId ┬Ę InputHandleIndex ┬Ę OutputHandleIndex ┬Ę
            // CodePageId(2) ┬Ę FolderId(8) ┬Ę AssociatedFlag(1). The dispatcher
            // consumed the leading RopId; consume LogonId here, then the
            // decoder reads the two handle indices + body.
            let _logon = cur.take_u8()?;
            let req = RopCreateMessageRequest::decode(cur)?;
            // Resolve the parent folder's backend id + kind from the INPUT
            // handle (the folder the client opened). For the synthetic root we
            // fall back to the drafts mailbox (drafts always save to \Drafts).
            let (parent_backend, parent_kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Folder { backend_id, kind } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            // Drafts are always mail-message objects even when composed inside
            // a Calendar/Contacts folder: Outlook composes IPM.Note drafts and
            // only a SaveChanges that morphs them to IPM.Appointment would land
            // them in a calendar mailbox. For Phase-2 we always create a draft
            // mail message; the calendar/contacts compose path is separate.
            let draft_mailbox_id = if parent_kind == FolderKind::Mail && !parent_backend.is_empty()
            {
                parent_backend.clone()
            } else {
                String::new()
            };
            // The draft's MAPI id is 0 until `RopSaveChangesMessage` persists
            // it to JMAP and we get the real backend id back; the success
            // envelope still carries HasMessageId=1 with a placeholder so the
            // client's output handle is valid. We track `is_new=true` on the
            // handle so a later SaveChanges drives `Email/set create`.
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    req.output_handle_index,
                    Handle::Message {
                        backend_id: String::new(),
                        mailbox_id: draft_mailbox_id.clone(),
                        kind: FolderKind::Mail,
                        is_new: true,
                    },
                );
            });
            let placeholder_mid = 0u64;
            RopCreateMessageSuccess {
                output_handle_index: req.output_handle_index,
                return_value: RopErrorCode::Success,
                has_message_id: 1,
                message_id: placeholder_mid,
            }
            .encode(out);
        }
        RopId::ROP_SAVE_CHANGES_MESSAGE => {
            // ┬¦2.2.6.3.1: LogonId ┬Ę ResponseHandleIndex ┬Ę InputHandleIndex ┬Ę
            // SaveFlags(1).
            let _logon = cur.take_u8()?;
            // RopHeader's 3rd byte is unused here; the decoder's `_header_handle`
            // param accommodates the optional ignored handle, but the spec wire
            // after LogonId is ResponseHandleIndex ┬Ę InputHandleIndex ┬Ę
            // SaveFlags, so we pass a 0 sentinel (the decoder ignores it).
            let req = RopSaveChangesMessageRequest::decode(cur, 0)?;
            // Resolve the message handle: must be `is_new` to drive an
            // Email/set create. An already-saved message is a no-op success
            // (Outlook re-saves after edits; an update is a separate Phase-3
            // SetProperties + SaveChanges sequence, treated here as idempotent
            // success to keep the client's state machine advancing).
            let (backend_id, mailbox_id, is_new) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        is_new,
                        ..
                    } => (backend_id.clone(), mailbox_id.clone(), *is_new),
                    _ => (String::new(), String::new(), false),
                })
                .unwrap_or((String::new(), String::new(), false));
            // Extract body content from any dirty stream owned by this message
            // whose tag is a message body (PR_BODY / PR_BODY_HTML /
            // PR_RTF_COMPRESSED). The stream holds staged bytes that must be
            // persisted via JMAP Email/set bodyValues before the message is
            // considered saved. If no dirty body stream, body_bytes will be
            // None and the downstream create-email path proceeds without a body
            # patch (the client may have supplied body in the UI or via other
            # means).
            let body_bytes = sessions
                .with_session_mut(session_id, |s| {
                    s.handles.values().find_map(|h| {
                        match h {
                            Handle::Stream {
                                source_handle_index,
                                property_tag,
                                is_dirty,
                                read_only,
                                data,
                                ..
                            } if *is_dirty
                                && !*read_only
                                && *source_handle_index == req.input_handle_index
                                && store::is_body_stream_tag(property_tag) =>
                            {
                                // Return the raw stream bytes; the caller will
                                // decode based on property type.
                                data.clone()
                            }
                            _ => None,
                        }
                    })
                })
                .transpose()
                .unwrap_or(None);
            let outcome: RopErrorCode;
            let saved_mid: u64;
            if let Some(bytes) = &body_bytes {
                // We have a dirty body stream — persist the bytes via JMAP Blob/upload,
                # then include the blobId in the Email/set create call so the bytes
                # are not dropped. This replaces the previous approach of embedding
                # body text directly, which could lose formatting or truncate.
                let body_value = String::from_utf8_lossy(bytes).to_string();
                let blob_id = jc
                    .upload_blob(&account_id, bytes.as_slice(), Some("message-body"), username, pw)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(
                            error = %e,
                            "Blob/upload failed for message body, falling back to text embedding"
                        );
                        // Fall back to embedding the body text directly
                        // (may lose some formatting but at least doesn't drop the body)
                        return; // Will fall through to the else branch below
                    });
                // Build the Email/set create object with bodyValues referencing the blob.
                let email_obj = serde_json::json!({
                    "mailboxIds": { (mailbox_id.clone()): true },
                    "keywords": { "$draft": true },
                    "bodyValues": {
                        "text": body_value.clone(),
                        "html": Some(body_value),
                        "blobId": blob_id,
                    },
                });
                // Create the email via JMAP Email/set with the body patch.
                let account_id_val = account_id.clone();
                match jc.create_email(&account_id_val, &email_obj, username, pw).await {
                    Ok(new_id) => {
                        saved_mid = store::message_id_from_jmap(&new_id);
                        // Promote the handle to a saved, non-new message.
                        sessions.with_session_mut(session_id, |s| {
                            if let Some(Handle::Message {
                                backend_id: bid,
                                mailbox_id: mid,
                                is_new: new_flag,
                                ..
                            }) = s.handles.get_mut(&req.input_handle_index)
                            {
                                *bid = new_id.clone();
                                *mid = mailbox_id.clone();
                                *new_flag = false;
                            }
                        });
                        RopSaveChangesMessageSuccess {
                            response_handle_index: req.response_handle_index,
                            return_value: RopErrorCode::Success,
                            input_handle_index: req.input_handle_index,
                            message_id: saved_mid,
                        }
                        .encode(out);
                        return Ok(());
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "JMAP Email/set create failed for save-changes-message with body"
                        );
                        // Fall through to try without blob reference
                    }
                }
            } else {
                // No dirty body stream — proceed with a plain draft create,
                # as the body will be handled by the client UI or other path.
                match (jmap, password, is_new) {
                (Some(jc), Some(pw), true) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        outcome = RopErrorCode::NotFound;
                        saved_mid = store::message_id_from_jmap(&backend_id);
                    } else {
                        // Resolve the drafts mailbox id if the create-message
                        // handle didn't carry one (root folder).
                        let mbox = if mailbox_id.is_empty() {
                            resolve_drafts_mailbox(jc, &account_id, username, pw).await
                        } else {
                            mailbox_id.clone()
                        };
                        let email_obj = serde_json::json!({
                            "mailboxIds": { (mbox.clone()): true },
                            "keywords": { "$draft": true },
                        });
                        match jc.create_email(&account_id, email_obj, username, pw).await {
                            Ok(new_id) => {
                                saved_mid = store::message_id_from_jmap(&new_id);
                                // Promote the handle to a saved, non-new message.
                                sessions.with_session_mut(session_id, |s| {
                                    if let Some(Handle::Message {
                                        backend_id: bid,
                                        mailbox_id: mid,
                                        is_new,
                                        ..
                                    }) = s.handle_mut(req.input_handle_index)
                                    {
                                        *bid = new_id.clone();
                                        *mid = mbox;
                                        *is_new = false;
                                    }
                                });
                                outcome = RopErrorCode::Success;
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP Email/set create (draft) failed");
                                outcome = RopErrorCode::DiskError;
                                saved_mid = 0;
                            }
                        }
                    }
                }
                (None, _, _) => {
                    outcome = RopErrorCode::NotFound;
                    saved_mid = 0;
                }
                (_, None, _) => {
                    outcome = RopErrorCode::AccessDenied;
                    saved_mid = 0;
                }
                (_, _, false) => {
                    // Already saved: idempotent success echoing the existing id.
                    outcome = RopErrorCode::Success;
                    saved_mid = store::message_id_from_jmap(&backend_id);
                }
            }
            RopSaveChangesMessageSuccess {
                response_handle_index: req.response_handle_index,
                return_value: outcome,
                input_handle_index: req.input_handle_index,
                message_id: saved_mid,
            }
            .encode(out);
        }
        RopId::ROP_DELETE_MESSAGES => {
            // ┬¦2.2.4.11.1: LogonId ┬Ę InputHandleIndex ┬Ę WantAsynchronous(1)
            // ┬Ę NotifyNonRead(1) ┬Ę MessageIdCount(2) ┬Ę MessageIds[count├Ś8].
            let _logon = cur.take_u8()?;
            let req = RopDeleteMessagesRequest::decode(cur)?;
            // The input handle is the source folder; resolve its backend id so
            // we can enumerate the folder to map MAPI ids -> JMAP ids.
            let (parent_backend, parent_kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Folder { backend_id, kind } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            let outcome: RopErrorCode;
            let partial: u8;
            match (
                jmap,
                password,
                parent_kind == FolderKind::Mail && !parent_backend.is_empty(),
            ) {
                (_, _, false) => {
                    // Non-mail folder: Email/destroy is conceptually invalid
                    // (MAPI_E_NO_SUPPORT), independent of backend availability,
                    // so a folder-kind check wins over the no-backend gate.
                    outcome = RopErrorCode::NoSupport;
                    partial = 0;
                }
                (None, _, _) => {
                    outcome = RopErrorCode::NotFound;
                    partial = 0;
                }
                (_, None, _) => {
                    outcome = RopErrorCode::AccessDenied;
                    partial = 0;
                }
                (Some(jc), Some(pw), true) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        outcome = RopErrorCode::NotFound;
                        partial = 0;
                    } else {
                        match jc
                            .list_email_ids_in_mailbox(&account_id, &parent_backend, username, pw)
                            .await
                        {
                            Ok(all) => {
                                let want: std::collections::HashSet<u64> =
                                    req.message_ids.iter().copied().collect();
                                let to_destroy: Vec<String> = all
                                    .into_iter()
                                    .filter(|(jid, _)| {
                                        want.contains(&store::message_id_from_jmap(jid))
                                    })
                                    .map(|(jid, _)| jid)
                                    .collect();
                                let found = to_destroy.len();
                                if found == 0 {
                                    outcome = RopErrorCode::Success;
                                    partial = 0;
                                } else {
                                    match jc
                                        .destroy_emails(&account_id, &to_destroy, username, pw)
                                        .await
                                    {
                                        Ok(destroyed) => {
                                            outcome = RopErrorCode::Success;
                                            // PartialCompletion=1 when the
                                            // server did not destroy every
                                            // requested id (some weren't in
                                            // this folder OR `notDestroyed`
                                            // rejected a subset). MS-OXCROPS
                                            // ┬¦2.2.4.11.2: a value of 1 means
                                            // the operation completed at least
                                            // one but not all of the requested
                                            // messages.
                                            partial = if destroyed == req.message_ids.len()
                                                && found == req.message_ids.len()
                                            {
                                                0
                                            } else {
                                                1
                                            };
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "JMAP Email/destroy failed"
                                            );
                                            outcome = RopErrorCode::DiskError;
                                            partial = 0;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP list for delete failed");
                                outcome = RopErrorCode::DiskError;
                                partial = 0;
                            }
                        }
                    }
                }
            }
            RopDeleteMessagesResponse {
                input_handle_index: req.input_handle_index,
                return_value: outcome,
                partial_completion: partial,
            }
            .encode(out);
        }
        RopId::ROP_MOVE_COPY_MESSAGES => {
            // ┬¦2.2.4.6.1: LogonId ┬Ę SourceHandleIndex ┬Ę DestHandleIndex ┬Ę
            // MessageIdCount(2) ┬Ę MessageIds[count├Ś8] ┬Ę WantAsynchronous(1)
            // ┬Ę WantCopy(1).
            let _logon = cur.take_u8()?;
            let req = RopMoveCopyMessagesRequest::decode_after_ropid(cur)?;
            // Resolve the source + dest folder backend ids.
            let (src_backend, src_kind) = sessions
                .with_handle(session_id, req.source_handle_index, |h| match h {
                    Handle::Folder { backend_id, kind } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            let (dest_backend, dest_kind) = sessions
                .with_handle(session_id, req.dest_handle_index, |h| match h {
                    Handle::Folder { backend_id, kind } => (backend_id.clone(), *kind),
                    _ => (String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), FolderKind::Root));
            let outcome: RopErrorCode;
            let partial: u8;
            match (
                jmap,
                password,
                src_kind == FolderKind::Mail && !src_backend.is_empty(),
                dest_kind == FolderKind::Mail && !dest_backend.is_empty(),
            ) {
                (_, _, false, _) | (_, _, _, false) => {
                    // Either endpoint is not a mail folder: the move/copy is
                    // conceptually invalid (MAPI_E_NO_SUPPORT) and that beats the
                    // backend-availability gate so Outlook surfaces the structural
                    // error rather than a misleading NotFound.
                    outcome = RopErrorCode::NoSupport;
                    partial = 0;
                }
                (None, _, _, _) => {
                    outcome = RopErrorCode::NotFound;
                    partial = 0;
                }
                (_, None, _, _) => {
                    outcome = RopErrorCode::AccessDenied;
                    partial = 0;
                }
                (Some(jc), Some(pw), true, true) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        outcome = RopErrorCode::NotFound;
                        partial = 0;
                    } else {
                        match jc
                            .list_email_ids_in_mailbox(&account_id, &src_backend, username, pw)
                            .await
                        {
                            Ok(all) => {
                                let want: std::collections::HashSet<u64> =
                                    req.message_ids.iter().copied().collect();
                                let jids: Vec<String> = all
                                    .into_iter()
                                    .filter(|(jid, _)| {
                                        want.contains(&store::message_id_from_jmap(jid))
                                    })
                                    .map(|(jid, _)| jid)
                                    .collect();
                                if jids.is_empty() {
                                    outcome = RopErrorCode::Success;
                                    partial = if req.message_ids.is_empty() { 0 } else { 1 };
                                } else if req.want_copy != 0 {
                                    match jc
                                        .copy_emails(
                                            &account_id,
                                            &jids,
                                            &dest_backend,
                                            username,
                                            pw,
                                        )
                                        .await
                                    {
                                        Ok(n) => {
                                            outcome = RopErrorCode::Success;
                                            // PartialCompletion=1 unless the
                                            // server created a copy for every
                                            // requested id (found + processed).
                                            // MS-OXCROPS ┬¦2.2.4.6.4.
                                            partial =
                                                if n == req.message_ids.len() { 0 } else { 1 };
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "JMAP copy failed");
                                            outcome = RopErrorCode::DiskError;
                                            partial = 0;
                                        }
                                    }
                                } else {
                                    match jc
                                        .move_emails(
                                            &account_id,
                                            &jids,
                                            &dest_backend,
                                            username,
                                            pw,
                                        )
                                        .await
                                    {
                                        Ok(n) => {
                                            outcome = RopErrorCode::Success;
                                            partial =
                                                if n == req.message_ids.len() { 0 } else { 1 };
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "JMAP move failed");
                                            outcome = RopErrorCode::DiskError;
                                            partial = 0;
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP list for move/copy failed");
                                outcome = RopErrorCode::DiskError;
                                partial = 0;
                            }
                        }
                    }
                }
            }
            RopMoveCopyMessagesResponse {
                source_handle_index: req.source_handle_index,
                return_value: outcome,
                partial_completion: partial,
            }
            .encode(out);
        }
        RopId::ROP_SUBMIT_MESSAGE => {
            // ┬¦2.2.7.1.1: LogonId ┬Ę InputHandleIndex ┬Ę SubmitFlags(1).
            let _logon = cur.take_u8()?;
            let req = RopSubmitMessageRequest::decode_after_ropid(cur)?;
            // Resolve the message handle (must be a saved, non-new draft with a
            // real backend id) so we can drive EmailSubmission/set.
            let (backend_id, is_new) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id, is_new, ..
                    } => (backend_id.clone(), *is_new),
                    _ => (String::new(), false),
                })
                .unwrap_or((String::new(), false));
            let outcome: RopErrorCode = match (jmap, password, is_new, !backend_id.is_empty()) {
                (_, _, true, _) => RopErrorCode::InvalidParameter, // unsaved draft
                (_, _, _, false) => RopErrorCode::NotFound,        // no backend id
                (None, _, false, true) => RopErrorCode::NotFound,  // backend id but no JMAP client
                (_, None, false, true) => RopErrorCode::AccessDenied,
                (Some(jc), Some(pw), false, true) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else {
                        // Fetch the full email to recover the envelope (from/to).
                        match jc.get_email(&account_id, &backend_id, username, pw).await {
                            Ok(Some(e)) => {
                                let from_addr = e
                                    .from
                                    .as_ref()
                                    .and_then(|v| v.first())
                                    .and_then(|a| a.email.clone())
                                    .unwrap_or_else(|| username.to_string());
                                let rcpts = email_recipients(&e);
                                if rcpts.is_empty() {
                                    RopErrorCode::InvalidParameter
                                } else {
                                    match jc
                                        .submit_existing_email(
                                            &account_id,
                                            &backend_id,
                                            &from_addr,
                                            &rcpts,
                                            username,
                                            pw,
                                        )
                                        .await
                                    {
                                        Ok(()) => RopErrorCode::Success,
                                        Err(e) => {
                                            tracing::warn!(error = %e, "JMAP submit failed");
                                            RopErrorCode::DiskError
                                        }
                                    }
                                }
                            }
                            _ => RopErrorCode::NotFound,
                        }
                    }
                }
            };
            RopSubmitMessageResponse {
                input_handle_index: req.input_handle_index,
                return_value: outcome,
            }
            .encode(out);
        }
        RopId::ROP_TRANSPORT_SEND => {
            // ┬¦2.2.7.6.1: LogonId ┬Ę InputHandleIndex. Identical send path to
            // RopSubmitMessage on the gateway (both drive EmailSubmission/set
            // against the saved draft referenced by the input handle); the
            // difference is purely client-side (TransportSend carries a
            // completion callback property set which we return empty).
            let _logon = cur.take_u8()?;
            let req = RopTransportSendRequest::decode_after_ropid(cur)?;
            let (backend_id, is_new) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id, is_new, ..
                    } => (backend_id.clone(), *is_new),
                    _ => (String::new(), false),
                })
                .unwrap_or((String::new(), false));
            let outcome: RopErrorCode = match (jmap, password, is_new, !backend_id.is_empty()) {
                (_, _, true, _) => RopErrorCode::InvalidParameter, // unsaved draft
                (_, _, _, false) => RopErrorCode::NotFound,        // no backend id
                (None, _, false, true) => RopErrorCode::NotFound,
                (_, None, false, true) => RopErrorCode::AccessDenied,
                (Some(jc), Some(pw), false, true) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else {
                        match jc.get_email(&account_id, &backend_id, username, pw).await {
                            Ok(Some(e)) => {
                                let from_addr = e
                                    .from
                                    .as_ref()
                                    .and_then(|v| v.first())
                                    .and_then(|a| a.email.clone())
                                    .unwrap_or_else(|| username.to_string());
                                let rcpts = email_recipients(&e);
                                if rcpts.is_empty() {
                                    RopErrorCode::InvalidParameter
                                } else {
                                    match jc
                                        .submit_existing_email(
                                            &account_id,
                                            &backend_id,
                                            &from_addr,
                                            &rcpts,
                                            username,
                                            pw,
                                        )
                                        .await
                                    {
                                        Ok(()) => RopErrorCode::Success,
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "JMAP transport-send failed"
                                            );
                                            RopErrorCode::DiskError
                                        }
                                    }
                                }
                            }
                            _ => RopErrorCode::NotFound,
                        }
                    }
                }
            };
            if outcome == RopErrorCode::Success {
                RopTransportSendSuccess {
                    input_handle_index: req.input_handle_index,
                    return_value: outcome,
                    no_properties_returned: 1,
                    property_value_count: 0,
                }
                .encode(out);
            } else {
                RopTransportSendFailure {
                    input_handle_index: req.input_handle_index,
                    return_value: outcome,
                }
                .encode(out);
            }
        }
        RopId::ROP_SET_PROPERTIES => {
            // MS-OXCROPS 2.2.8.6.1: the 3-byte header after the dispatcher's
            // RopId is LogonId + InputHandleIndex. The spec has NO
            // ResponseHandleIndex byte ŌĆö a phantom read here would steal the
            // low byte of PropertyValueSize and abort every SetProperties with
            // InvalidParameter. The shared success/failure envelope echoes
            // the InputHandleIndex as HandleIndex (qodo #1, cubic #16/#22).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSetPropertiesRequest::decode(cur)?;
            // Resolve the Message handle (backend_id = JMAP email id,
            // mailbox_id = the JMAP mailbox the email lives in, used to range
            // the published modification event). An Attachment handle is NOT
            // settable through MAPI in this phase (the body/MIME-rewrite
            // bridge is pending): return a typed `NoSupport` rather than an
            // empty backend id masquerading as `NotFound`, so the client gets
            // a meaningful result and `handle_index` is correct.
            enum SetPropsTarget {
                Message {
                    backend_id: String,
                    mailbox_id: String,
                },
                Attachment,
                None,
            }
            let target = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        ..
                    } => SetPropsTarget::Message {
                        backend_id: backend_id.clone(),
                        mailbox_id: mailbox_id.clone(),
                    },
                    Handle::Attachment { .. } => SetPropsTarget::Attachment,
                    _ => SetPropsTarget::None,
                })
                .unwrap_or(SetPropsTarget::None);
            if matches!(target, SetPropsTarget::Attachment) {
                // Attachment property write via MAPI is not directly supported;
                # instead, we upload the attachment blob via JMAP Blob/upload and
                # then reference it in the Email/set update. This enables
                # MAPI-compose-with-attachment scenarios (gap #3/#4).
                let store::PropertyPatch { properties, .. } =
                    store::set_values_to_patch(&req.property_values);
                // Look for an attachment blob property (PR_ATTACH_DATA_BIN)
                if let Some(attach_prop) = properties
                    .iter()
                    .find(|p| p.tag == store::PR_ATTACH_DATA_BIN)
                {
                    let data = &attach_prop.value;
                    let name = attach_prop
                        .value
                        .as_object()
                        .and_then(|o| o.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("attachment.bin");
                    // Upload the blob via JMAP
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if !account_id.is_empty() {
                        let blob_id = jc
                            .upload_blob(&account_id, data, Some(name), username, pw)
                            .await;
                        match blob_id {
                            Ok(blob_id) => {
                                // Successfully uploaded - now update the email to reference the blob
                                let update = serde_json::json!({
                                    "accountId": account_id,
                                    "blobId": blob_id,
                                });
                                match jc.update_email_checked(&account_id, &update, username, pw).await {
                                    Ok(_) => {
                                        // Attachment persisted; surface success
                                        RopPropertyWriteSuccess {
                                            rop_id,
                                            handle_index: input_handle_index,
                                            return_value: RopErrorCode::Success,
                                            problems: vec![],
                                        }
                                        .encode(out);
                                        return Ok(());
                                    }
                                    Err(e) => {
                                        tracing::warn!(
                                            error = %e,
                                            "JMAP Email/set update after Blob/upload failed"
                                        );
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Blob/upload failed for attachment in RopSetProperties"
                                );
                            }
                        }
                    }
                }
                // If we get here, either no attachment prop found or upload failed;
                # surface NoSupport so the client gets a meaningful error.
                RopPropertyWriteSuccess {
                    rop_id,
                    handle_index: input_handle_index,
                    return_value: RopErrorCode::NoSupport,
                    problems,
                }
                .encode(out);
                return Ok(());
            }
            let (backend_id, mailbox_id) = match target {
                SetPropsTarget::Message {
                    backend_id,
                    mailbox_id,
                } => (backend_id, mailbox_id),
                _ => (String::new(), String::new()),
            };
            let store::PropertyPatch { patch, problems } =
                store::set_values_to_patch(&req.property_values);
            let return_value: RopErrorCode = match (jmap, password, backend_id.as_str()) {
                (_, _, "") => RopErrorCode::NotFound, // message handle not bound
                (None, _, _) => RopErrorCode::NotFound, // no JMAP backend configured
                (_, None, _) => RopErrorCode::AccessDenied, // no credentials
                (Some(jc), Some(pw), id) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else if patch.is_empty() {
                        // No translatable fields: the apply is a no-op
                        // success carrying only the per-property problems
                        // (e.g. read-only props reported NO_SUPPORT). This
                        // avoids a needless Email/set round-trip.
                        RopErrorCode::Success
                    } else {
                        // Inspect the Email/set response rather than masking
                        // a server-side failure as success: a per-id
                        // `notUpdated` entry or a method-level `error` becomes
                        // a MAPI DiskError so Outlook surfaces it (qodo #3/#5,
                        // cubic #23). The transport error is logged verbatim.
                        let update = serde_json::json!({ id: serde_json::Value::Object(patch) });
                        match jc
                            .update_email_checked(&account_id, &update, username, pw)
                            .await
                        {
                            Ok(outcome) => outcome_to_code(outcome, "Email/set update"),
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP Email/set update failed");
                                RopErrorCode::DiskError
                            }
                        }
                    }
                }
            };
            // On a real apply, publish an ItemModified so MAPI NotificationWait
            // (and the EWS subscription feed sharing the same manager) sees the
            // change instead of forcing the client to re-poll (qodo #9, cubic
            // #30, audit ┬¦2e).
            if return_value == RopErrorCode::Success {
                publish_item_modified(subscription_manager, username, &mailbox_id, &backend_id);
            }
            RopPropertyWriteSuccess {
                rop_id,
                handle_index: input_handle_index,
                return_value,
                problems,
            }
            .encode(out);
        }
        RopId::ROP_DELETE_PROPERTIES => {
            // MS-OXCROPS 2.2.8.8.1: the 3-byte header is LogonId +
            // InputHandleIndex ŌĆö NO ResponseHandleIndex byte (same P0 fix as
            // SetProperties; a phantom read here steals the low byte of
            // PropertyTagCount and aborts every DeleteProperties). The
            // envelope echoes InputHandleIndex as HandleIndex
            // (qodo #1, cubic #16/#22).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopDeletePropertiesRequest::decode(cur)?;
            let (backend_id, mailbox_id) = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        ..
                    } => (backend_id.clone(), mailbox_id.clone()),
                    _ => (String::new(), String::new()),
                })
                .unwrap_or((String::new(), String::new()));
            let store::PropertyPatch { patch, problems } =
                store::delete_tags_to_patch(&req.property_tags);
            let return_value: RopErrorCode = match (jmap, password, backend_id.as_str()) {
                (_, _, "") => RopErrorCode::NotFound, // message handle not bound
                (None, _, _) => RopErrorCode::NotFound, // no JMAP backend configured
                (_, None, _) => RopErrorCode::AccessDenied, // no credentials
                (Some(jc), Some(pw), id) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else if patch.is_empty() {
                        RopErrorCode::Success
                    } else {
                        let update = serde_json::json!({ id: serde_json::Value::Object(patch) });
                        match jc
                            .update_email_checked(&account_id, &update, username, pw)
                            .await
                        {
                            Ok(outcome) => outcome_to_code(outcome, "Email/set update"),
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP Email/set update (delete) failed");
                                RopErrorCode::DiskError
                            }
                        }
                    }
                }
            };
            if return_value == RopErrorCode::Success {
                publish_item_modified(subscription_manager, username, &mailbox_id, &backend_id);
            }
            RopPropertyWriteSuccess {
                rop_id,
                handle_index: input_handle_index,
                return_value,
                problems,
            }
            .encode(out);
        }
        RopId::ROP_COPY_TO => {
            // MS-OXCMAPIHTTP / MS-OXCROPS 2.2.8.12.1: post-RopId byte is
            // LogonId, then the decoder reads SourceHandleIndex +
            // DestHandleIndex, then WantAsynchronous + WantSubObjects +
            // CopyFlags + ExcludedTagCount + ExcludedTags. (The decoder
            // reading the two handle indices matches the established
            // MoveCopy convention ŌĆö the dispatcher consumes LogonId, the
            // decoder consumes handles + body ŌĆö so coderabbit #5 is INVALID;
            // no change to the codec ordering is made.)
            let _logon = cur.take_u8()?;
            let req = RopCopyToRequest::decode(cur)?;
            // Resolve both the source and destination Message handles to
            // their JMAP email ids; capture the destination mailbox_id to
            // range the modification event.
            let src_id = sessions
                .with_handle(session_id, req.source_handle_index, |h| match h {
                    Handle::Message { backend_id, .. } => Some(backend_id.clone()),
                    _ => None,
                })
                .flatten()
                .unwrap_or_default();
            let (dst_id, dst_mailbox_id) = sessions
                .with_handle(session_id, req.dest_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        ..
                    } => (backend_id.clone(), mailbox_id.clone()),
                    _ => (String::new(), String::new()),
                })
                .unwrap_or((String::new(), String::new()));
            // RopCopyTo copies the message proper. The supported copy is the
            // scalar property patch (subject, importance, follow-up flag)
            // read off the source email and applied to the destination via
            // Email/set. ExcludedTags IS honoured: a requested exclusion in
            // the scalar set suppresses that property from the patch AND
            // records a MAPI_E_NO_SUPPORT PropertyProblem so the caller knows
            // the property was not copied (qodo #4/#8, cubic #12). A partial
            // copy (excluded tags, or a source with no scalar props) is a
            // spec-compliant Success with a populated problem array
            // (2.2.8.12.2 ŌĆö problems report per-property issues; the
            // aggregate ROP return value is Success).
            use crate::mapi::data::{PropertyProblem, PropertyTag, PropertyType};
            use crate::mapi::store::{PR_FLAG_STATUS, PR_IMPORTANCE, PR_SUBJECT};
            const ERR_NOT_COPIED: u32 = 0x8004_0102; // MAPI_E_NO_SUPPORT
            let excluded_ids: std::collections::HashSet<u16> =
                req.excluded_tags.iter().map(|t| t.property_id).collect();
            let excluded_subject = excluded_ids.contains(&PR_SUBJECT);
            let excluded_importance = excluded_ids.contains(&PR_IMPORTANCE);
            let excluded_flag = excluded_ids.contains(&PR_FLAG_STATUS);
            let mut problems: Vec<PropertyProblem> = Vec::new();
            if excluded_subject {
                problems.push(PropertyProblem {
                    index: 0,
                    tag: PropertyTag::new(PropertyType::PTYP_STRING, PR_SUBJECT),
                    error_code: ERR_NOT_COPIED,
                });
            }
            if excluded_importance {
                problems.push(PropertyProblem {
                    index: 1,
                    tag: PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_IMPORTANCE),
                    error_code: ERR_NOT_COPIED,
                });
            }
            if excluded_flag {
                problems.push(PropertyProblem {
                    index: 2,
                    tag: PropertyTag::new(PropertyType::PTYP_INTEGER32, PR_FLAG_STATUS),
                    error_code: ERR_NOT_COPIED,
                });
            }

            let return_value: RopErrorCode = match (
                jmap,
                password,
                src_id.as_str(),
                dst_id.as_str(),
            ) {
                (_, _, "", _) | (_, _, _, "") => RopErrorCode::NotFound, // source/dest not bound
                (None, _, _, _) => RopErrorCode::NotFound, // no JMAP backend configured
                (_, None, _, _) => RopErrorCode::AccessDenied, // no credentials
                (Some(jc), Some(pw), src, dst) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        RopErrorCode::NotFound
                    } else {
                        match jc.get_email(&account_id, src, username, pw).await {
                            Ok(Some(src_email)) => {
                                let mut patch = serde_json::Map::new();
                                if !excluded_subject && let Some(subj) = src_email.subject.as_ref()
                                {
                                    patch.insert(
                                        "subject".to_string(),
                                        serde_json::Value::String(subj.clone()),
                                    );
                                }
                                if let Some(kw) = src_email.keywords.as_ref() {
                                    let imp = kw.contains_key("$important");
                                    let flagged = kw.contains_key("$flagged");
                                    if !excluded_importance {
                                        patch.insert(
                                            "keywords/$important".to_string(),
                                            if imp {
                                                serde_json::json!(true)
                                            } else {
                                                serde_json::Value::Null
                                            },
                                        );
                                    }
                                    if !excluded_flag {
                                        patch.insert(
                                            "keywords/$flagged".to_string(),
                                            if flagged {
                                                serde_json::json!(true)
                                            } else {
                                                serde_json::Value::Null
                                            },
                                        );
                                    }
                                }
                                if patch.is_empty() {
                                    RopErrorCode::Success
                                } else {
                                    let update = serde_json::json!({ dst: serde_json::Value::Object(patch) });
                                    match jc
                                        .update_email_checked(&account_id, &update, username, pw)
                                        .await
                                    {
                                        Ok(outcome) => outcome_to_code(outcome, "Email/set copy"),
                                        Err(e) => {
                                            tracing::warn!(
                                                error = %e,
                                                "JMAP Email/set update (copy) failed"
                                            );
                                            RopErrorCode::DiskError
                                        }
                                    }
                                }
                            }
                            Ok(None) => RopErrorCode::NotFound,
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP get_email (copy source) failed");
                                RopErrorCode::DiskError
                            }
                        }
                    }
                }
            };
            // 2.2.8.12.2 echoes the SourceHandleIndex in the response.
            if return_value == RopErrorCode::Success {
                publish_item_modified(subscription_manager, username, &dst_mailbox_id, &dst_id);
            }
            RopCopyToSuccess {
                rop_id,
                handle_index: req.source_handle_index,
                return_value,
                problems,
            }
            .encode(out);
        }
        RopId::ROP_CREATE_ATTACHMENT => {
            // ┬¦2.2.6.13.1: header-only request (RopId ┬Ę LogonId ┬Ę
            // InputHandleIndex ┬Ę OutputHandleIndex). The dispatcher consumed
            // the RopId; decode the trailing LogonId ┬Ę Input ┬Ę Output via
            // RopHeader4 (the same helper that handles the OutputHandleIndex
            // field). The input handle MUST be a Message the client is
            // composing the attachment against in the store.
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = RopCreateAttachmentRequest {
                input_handle_index: h4.input_handle_index,
                output_handle_index: h4.output_handle_index,
            };
            // Resolve owning message; must be a mail Message (calendar/contact
            // attachment composition over MAPI is not supported in this phase).
            let (email_id, mailbox_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        kind,
                        ..
                    } => (backend_id.clone(), mailbox_id.clone(), *kind),
                    _ => (String::new(), String::new(), FolderKind::Root),
                })
                .unwrap_or((String::new(), String::new(), FolderKind::Root));
            if kind != FolderKind::Mail || email_id.is_empty() {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            // The body write-back bridge that would persist a MAPI-composed
            // attachment's bytes (RopWriteStream on PR_ATTACH_DATA_BIN ŌåÆ
            // JMAP Blob/upload ŌåÆ Email/set patching attachments[]) is not yet
            // wired. Surface `NoSupport` so a client gets a typed error rather
            // than `NotFound`, and do not allocate a phantom attachment
            // handle the save could not commit. (Audit ┬¦2a write-back follow-up)
            let _ = attachment_manager;
            let _ = mailbox_id;
            RopErrorResponse {
                rop_id,
                output_handle_index: req.output_handle_index,
                return_value: RopErrorCode::NoSupport,
            }
            .encode(out);
        }
        RopId::ROP_DELETE_ATTACHMENT => {
            // ┬¦2.2.6.14.1: RopId ┬Ę LogonId ┬Ę InputHandleIndex ┬Ę
            // AttachmentID(4 LE). The dispatcher consumed only RopId, so
            // consume LogonId here then let `decode_after_ropid` read
            // InputHandleIndex + AttachmentID (body-only ŌĆö per-codec contract).
            let _logon = cur.take_u8()?;
            let req = RopDeleteAttachmentRequest::decode_after_ropid(cur)?;
            // The input handle MUST be a mail Message; AttachmentID is the
            // PR_ATTACH_NUM the client wants to remove.
            let (kind, email_id) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id, kind, ..
                    } => (*kind, backend_id.clone()),
                    _ => (FolderKind::Root, String::new()),
                })
                .unwrap_or((FolderKind::Root, String::new()));
            if kind != FolderKind::Mail || email_id.is_empty() {
                RopDeleteAttachmentResponse {
                    input_handle_index: req.input_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            // Deleting an attachment requires rewriting the owning message's
            // MIME so Stalwart drops the body part; that write-back bridge is
            // not wired, so surface `NoSupport`. (A MAPI-created attachment
            // that was staged but not persisted needs no teardown; one that was
            // saved would live in the gateway store and could be deleted via
            // `AttachmentManager::delete_attachments_for_item` once the read
            // path unions that store ŌĆö tracked with the body write-back bridge.)
            let _ = attachment_manager;
            RopDeleteAttachmentResponse {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::NoSupport,
            }
            .encode(out);
        }
        RopId::ROP_SAVE_CHANGES_ATTACHMENT => {
            // ┬¦2.2.6.15.1: RopId ┬Ę LogonId ┬Ę ResponseHandleIndex
            // ┬Ę InputHandleIndex ┬Ę SaveFlags(1). The dispatcher consumed only
            // RopId, so consume LogonId here then let `decode_after_ropid`
            // read ResponseHandleIndex ┬Ę InputHandleIndex ┬Ę SaveFlags
            // (body-only).
            let _logon = cur.take_u8()?;
            let req = RopSaveChangesAttachmentRequest::decode_after_ropid(cur)?;
            // The input handle MUST be an Attachment handle (opened by
            // RopCreateAttachment). A JMAP-native attachment opened by
            // RopOpenAttachment is immutable, so SaveChangesAttachment on it
            // is an idempotent Success (the client sometimes re-saves after a
            // read-only OpenAttachment); a not-yet-persisted MAPI-created
            // attachment needs the body write-back bridge (Blob/upload ŌåÆ
            // Email/set) which is not wired, so it surfaces `NoSupport`.
            let (is_jmap_native, email_id) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Attachment {
                        is_new, email_id, ..
                    } => (!is_new, email_id.clone()),
                    _ => (false, String::new()),
                })
                .unwrap_or((false, String::new()));
            if is_jmap_native {
                // Idempotent success on an immutable JMAP attachment: report
                // Success without persisting (there is nothing to write back).
                RopSaveChangesAttachmentResponse {
                    response_handle_index: req.response_handle_index,
                    return_value: RopErrorCode::Success,
                }
                .encode(out);
                return Ok(());
            }
            // MAPI-created attachment pending the body write-back bridge (Blob/upload).
            let _ = attachment_manager;
            let _ = email_id;
            // Extract any dirty body stream data and persist via JMAP Blob/upload
            let body_data = sessions
                .with_session_mut(session_id, |s| {
                    s.handles.values().find_map(|h| {
                        match h {
                            Handle::Stream {
                                source_handle_index,
                                property_tag,
                                is_dirty,
                                read_only,
                                data,
                                ..
                            } if *is_dirty
                                && !*read_only
                                && *source_handle_index == req.input_handle_index
                                && store::is_body_stream_tag(property_tag) =>
                            {
                                data.clone()
                            }
                            _ => None,
                        }
                    })
                })
                .transpose()
                .unwrap_or(None);
            if let Some(bytes) = &body_data {
                // Persist the attachment bytes via JMAP Blob/upload
                let account_id = jc
                    .get_account_id(username, pw)
                    .await
                    .ok()
                    .unwrap_or_default();
                if !account_id.is_empty() {
                    // Get the attachment name from the handle or use a default
                    let attachment_name = sessions
                        .with_handle(session_id, req.input_handle_index, |h| {
                            match h {
                                Handle::Attachment { .. } => Some("attachment.bin".to_string()),
                                _ => None,
                            }
                        })
                        .unwrap_or_else(|| "attachment.bin".to_string());
                    match jc.upload_blob(&account_id, bytes, Some(&attachment_name), username, pw).await {
                        Ok(blob_id) => {
                            // Successfully uploaded - now update the email to reference the blob
                            let update = serde_json::json!({
                                "accountId": account_id,
                                "blobId": blob_id,
                            });
                            match jc.update_email_checked(&account_id, &update, username, pw).await {
                                Ok(_) => {
                                    // Attachment persisted; surface success
                                    RopSaveChangesAttachmentResponse {
                                        response_handle_index: req.response_handle_index,
                                        return_value: RopErrorCode::Success,
                                    }
                                    .encode(out);
                                    return Ok(());
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "JMAP Email/set update after Blob/upload failed for attachment"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Blob/upload failed for attachment in RopSaveChangesAttachment"
                            );
                        }
                    }
                }
            }
            // If we get here without persisting, surface NoSupport so the client
            // gets a meaningful error rather than silent success.
            RopSaveChangesAttachmentResponse {
                response_handle_index: req.response_handle_index,
                return_value: RopErrorCode::NoSupport,
            }
            .encode(out);
        }
        RopId::ROP_OPEN_STREAM => {
            // 2.2.9.1.1: LogonId - InputHandleIndex - OutputHandleIndex
            // - PropertyTag(4) - OpenModeFlags(1) (a 4-byte RopHeader4 body
            // followed by the open-mode flag). The dispatcher consumed the
            // leading RopId; consume LogonId+Input+Output via
            // RopHeader4::decode_after_ropid here, then decode reads only the
            // body fields (PropertyTag + OpenModeFlags) so the codec never
            // re-takes dispatcher-owned header bytes (AGENTS.md convention).
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = RopOpenStreamRequest::decode_body(
                cur,
                h4.input_handle_index,
                h4.output_handle_index,
            )?;
            // Resolve the owning object from the input handle. A Mail Message
            // handle streams its body properties; an Attachment handle (opened
            // by `RopOpenAttachment`/`RopCreateAttachment`) streams that one
            // attachment's `PR_ATTACH_DATA_BIN` directly off the handle's cached
            // `blob_id`, so a message carrying several attachments streams the
            // *correct* one (the message-scoped `email_attachment_blob` path
            // below intentionally fails closed for >1 attachment).
            enum StreamOwner {
                Message(String, String, FolderKind),
                Attachment(String, String, FolderKind, String, Option<u64>),
                Other,
            }
            let owner = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        mailbox_id,
                        kind,
                        ..
                    } => StreamOwner::Message(backend_id.clone(), mailbox_id.clone(), *kind),
                    Handle::Attachment {
                        email_id,
                        mailbox_id,
                        kind,
                        blob_id,
                        size,
                        ..
                    } => StreamOwner::Attachment(
                        email_id.clone(),
                        mailbox_id.clone(),
                        *kind,
                        blob_id.clone(),
                        *size,
                    ),
                    _ => StreamOwner::Other,
                })
                .unwrap_or(StreamOwner::Other);
            // Fast path: an Attachment handle streaming PR_ATTACH_DATA_BIN (the
            // usual New Outlook "download this attachment" flow). The handle
            // already carries the JMAP blob id (Stalwart-assigned, captured at
            // `RopOpenAttachment`), so we install a read-only Stream resolved
            // to that specific blob without re-reading the message body. The
            // handle's captured `size` becomes `known_len` so `OpenStream`'s
            // `StreamSize` (and `RopGetStreamSize`) report the real length
            // before the blob is materialised, and the `max_attachment_bytes`
            // ceiling rejects an oversized blob with `NotEnoughMemory` before
            // any download ŌĆö matching the message-scoped path's enforcement.
            //
            // Gate strictly on `PR_ATTACH_DATA_BIN`/`PTYP_BINARY` so requests
            // for attachment *metadata* (`PR_ATTACH_LONG_FILENAME`, etc.) or a
            // type-mismatched data-bin never receive the binary payload; those
            // fall through to the legacy path which returns the property (or a
            // typed empty/error for unsupported tags). An empty `blob_id` means
            // there are no downloadable bytes (a not-yet-saved MAPI-created
            // attachment); fall through rather than packing the email id alone.
            if let StreamOwner::Attachment(email_id, mailbox_id, kind, blob_id, att_size) = &owner
                && kind == &FolderKind::Mail
                && !email_id.is_empty()
                && !blob_id.is_empty()
                && req.property_tag.property_id == store::PR_ATTACH_DATA_BIN
                && req.property_tag.property_type == crate::mapi::data::PropertyType::PTYP_BINARY
            {
                // Enforce the configured/spec attachment byte ceiling before
                // the download: a declared size over the cap is rejected with
                // `NotEnoughMemory` (item L / audit ┬¦2a), mirroring the
                // message-scoped RopOpenStream path. A `RopErrorResponse`
                // envelope is used (not the success shape) so no stream size
                // leaks past the error and the chain cursor stays aligned.
                let cap = cfg.max_attachment_bytes() as u64;
                if let Some(len) = att_size
                    && cap > 0
                    && *len > cap
                {
                    RopErrorResponse {
                        rop_id,
                        output_handle_index: req.output_handle_index,
                        return_value: RopErrorCode::NotEnoughMemory,
                    }
                    .encode(out);
                    return Ok(());
                }
                let known_len = *att_size;
                let packed = format!("{email_id}\x1F{blob_id}");
                sessions.with_session_mut(session_id, |s| {
                    s.set_handle(
                        req.output_handle_index,
                        Handle::Stream {
                            source_handle_index: req.input_handle_index,
                            kind: *kind,
                            backend_id: packed,
                            mailbox_id: mailbox_id.clone(),
                            property_tag: req.property_tag,
                            data: None,
                            known_len,
                            cursor: 0,
                            is_dirty: false,
                            read_only: true,
                        },
                    );
                });
                let initial_len = match known_len {
                    Some(len) => u32::try_from(len).unwrap_or(u32::MAX),
                    None => 0,
                };
                RopOpenStreamSuccess {
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::Success,
                    stream_size: initial_len,
                }
                .encode(out);
                return Ok(());
            }
            // Legacy message-scoped path: only a Mail Message handle carries
            // streamable bodies/attachments.
            let (src_backend, src_mailbox, src_kind) = match owner {
                StreamOwner::Message(b, m, k) => (b, m, k),
                StreamOwner::Attachment(..) => (String::new(), String::new(), FolderKind::Root),
                StreamOwner::Other => (String::new(), String::new(), FolderKind::Root),
            };
            // Guard: only mail messages carry streamable bodies/attachments;
            // a stream opened on a folder/table/calendar/contact handle has
            // no backing property and reports `NoSupport` (the spec mandates a
            // ROP-level error rather than a transport success with empty bytes,
            // so Outlook does not wait on a stream that will never return data).
            if src_kind != FolderKind::Mail || src_backend.is_empty() {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::NoSupport,
                }
                .encode(out);
                return Ok(());
            }
            // Fetch the full JMAP email once; body bytes are lifted from
            // bodyValues / htmlBody, and the attachment blob id is recorded for
            // lazy download on the first ReadStream. A missing JMAP backend or
            // credentials yields `NotFound` (Outlook treats it as "stream empty"
            // - better here than `Success` with a phantom size that would make
            // the client loop ReadStream returning zero bytes forever).
            // `known_len` carries the attachment's declared size (so OpenStream
            // / GetStreamSize report a real size before the first ReadStream
            // downloads the blob, and the download can be bounded up front).
            let (return_value, blob_id, data, read_only, known_len) = match (jmap, password) {
                (Some(jc), Some(pw)) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        (RopErrorCode::NotFound, String::new(), None, false, None)
                    } else {
                        match jc.get_email(&account_id, &src_backend, username, pw).await {
                            Ok(Some(e)) => {
                                // Try body property first (cheap, already in JSON).
                                match store::email_body_stream_bytes(&e, &req.property_tag) {
                                    Some(bytes) => (
                                        RopErrorCode::Success,
                                        String::new(),
                                        Some(bytes),
                                        false,
                                        None,
                                    ),
                                    None => {
                                        // Otherwise resolve an attachment blob id; capture its
                                        // declared size alongside so OpenStream/GetStreamSize can
                                        // report it without a premature blob download (coderabbit).
                                        match store::email_attachment_blob(&e, &req.property_tag) {
                                            Some(att) => (
                                                RopErrorCode::Success,
                                                att.blob_id.clone().unwrap_or_default(),
                                                None,
                                                true,
                                                att.size,
                                            ),
                                            None => {
                                                // Not a body / not an attachment-data
                                                // tag: open a typed-empty stream so a
                                                // ReadStream returns zero bytes rather
                                                // than NotFound (the property simply has
                                                // no value on this message).
                                                (
                                                    RopErrorCode::Success,
                                                    String::new(),
                                                    Some(Vec::new()),
                                                    false,
                                                    None,
                                                )
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => (RopErrorCode::NotFound, String::new(), None, false, None),
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP get_email (OpenStream) failed");
                                (RopErrorCode::DiskError, String::new(), None, false, None)
                            }
                        }
                    }
                }
                _ => (RopErrorCode::NotFound, String::new(), None, false, None),
            };
            if return_value != RopErrorCode::Success {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.output_handle_index,
                    return_value,
                }
                .encode(out);
                return Ok(());
            }
            // Cap a not-yet-downloaded attachment stream by the configured
            // `max_attachment_bytes` against the JMAP-declared `known_len`,
            // so OpenStream does not invite a fetch larger than the ceiling.
            let max_att = u64::try_from(cfg.max_attachment_bytes()).unwrap_or(u64::MAX);
            let initial_len: u32 = if let Some(b) = &data {
                u32::try_from(b.len()).unwrap_or(u32::MAX)
            } else if let Some(len) = known_len {
                if len > max_att {
                    RopErrorResponse {
                        rop_id,
                        output_handle_index: req.output_handle_index,
                        return_value: RopErrorCode::NotEnoughMemory,
                    }
                    .encode(out);
                    return Ok(());
                }
                u32::try_from(len).unwrap_or(u32::MAX)
            } else {
                0
            };
            // Pack the attachment blob id (if any) alongside the email id so the
            // stream can resolve it without re-reading the source handle (which
            // the client may have released between OpenStream and ReadStream).
            let packed_backend = if blob_id.is_empty() {
                src_backend.clone()
            } else {
                format!("{}\x1F{blob_id}", src_backend)
            };
            sessions.with_session_mut(session_id, |s| {
                s.set_handle(
                    req.output_handle_index,
                    Handle::Stream {
                        source_handle_index: req.input_handle_index,
                        kind: src_kind,
                        backend_id: packed_backend,
                        mailbox_id: src_mailbox.clone(),
                        property_tag: req.property_tag,
                        data,
                        known_len,
                        cursor: 0,
                        is_dirty: false,
                        read_only,
                    },
                );
            });
            RopOpenStreamSuccess {
                output_handle_index: req.output_handle_index,
                return_value: RopErrorCode::Success,
                stream_size: initial_len,
            }
            .encode(out);
        }
        RopId::ROP_READ_STREAM => {
            // 2.2.9.2.1: LogonId - InputHandleIndex - ByteCount(2)
            // - [MaximumByteCount(4) if ByteCount == 0xBABE].
            let _logon = cur.take_u8()?;
            let req = RopReadStreamRequest::decode_after_ropid(cur)?;
            let max_bytes = match req.max_bytes() {
                Ok(m) => m,
                Err(e) => {
                    // Encode the ROP-level InvalidParameter once and return Ok so
                    // the outer Execute loop does not emit a SECOND error ROP
                    // (qodo #1, coderabbit critical): taking the remaining bytes
                    // keeps the chain cursor aligned for the following ROP.
                    let _ = cur.take_remaining();
                    RopErrorResponse {
                        rop_id,
                        output_handle_index: req.input_handle_index,
                        return_value: RopErrorCode::InvalidParameter,
                    }
                    .encode(out);
                    let _ = e;
                    return Ok(());
                }
            };
            // Pull the stream's owned state (backend id, mailbox id, cursor,
            // read-only flag, has-data) from the snapshot handle; the body/
            // attachment bytes are materialised under the write lock below so the
            // cursor advance is atomic.
            let stream_meta = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Stream {
                        backend_id,
                        mailbox_id,
                        cursor,
                        read_only,
                        data,
                        ..
                    } => (
                        backend_id.clone(),
                        mailbox_id.clone(),
                        *cursor,
                        *read_only,
                        data.is_some(),
                    ),
                    _ => (String::new(), String::new(), 0u64, false, false),
                })
                .unwrap_or((String::new(), String::new(), 0, false, false));
            // A read-only attachment stream whose blob has not been downloaded
            // yet (backend id carries the packed `<emailId>\x1F<blobId>` and
            // `data` is still None) triggers a single `download_blob` now; the
            // bytes attach to the handle under the write lock below. Once `data`
            // is populated, subsequent paginated reads skip the network round
            // trip entirely (qodo #3 / coderabbit performance).
            let downloaded = if stream_meta.3 && stream_meta.0.contains('\x1F') && !stream_meta.4 {
                materialise_attachment_blob(jmap, password, username, &stream_meta.0, cfg).await
            } else {
                None
            };
            // Under the write lock: attach the downloaded bytes (if any) on
            // first read, then take a slice of `data` from `cursor` bounded by
            // `max_bytes` (capped at the 2-byte DataSize wire limit) and advance
            // the cursor by exactly the number of bytes returned on the wire.
            let (return_value, data) = sessions
                .with_session_mut(session_id, |s| {
                    let Some(Handle::Stream { data, cursor, .. }) =
                        s.handle_mut(req.input_handle_index)
                    else {
                        return (RopErrorCode::NotFound, Vec::new());
                    };
                    if data.is_none() {
                        if let Some(bytes) = downloaded.as_ref() {
                            *data = Some(bytes.clone());
                        } else {
                            // Body stream opened with empty data, or attachment
                            // download failed: attach an empty buffer so a
                            // subsequent ReadStream returns zero bytes rather
                            // than spinning the cursor guard again.
                            *data = Some(Vec::new());
                        }
                    }
                    let buf = data.get_or_insert_with(Vec::new);
                    let start = usize::try_from(*cursor).unwrap_or(buf.len()).min(buf.len());
                    // The response carries a 2-byte DataSize, so a single read
                    // can deliver at most u16::MAX bytes. Clamp the request's
                    // max here so the cursor advances by exactly the emitted
                    // length: a MaximumByteCount > 65535 (0xBABE extended form)
                    // must NOT skip the bytes the response cannot carry.
                    let want = (max_bytes as usize).min(usize::from(u16::MAX));
                    let end = (start + want).min(buf.len());
                    let chunk = buf[start..end].to_vec();
                    *cursor = u64::try_from(end).unwrap_or(u64::MAX);
                    (RopErrorCode::Success, chunk)
                })
                .unwrap_or((RopErrorCode::NotFound, Vec::new()));
            RopReadStreamSuccess {
                input_handle_index: req.input_handle_index,
                return_value,
                data,
            }
            .encode(out);
        }
        RopId::ROP_WRITE_STREAM => {
            // ┬¦2.2.9.3.1: LogonId ┬Ę InputHandleIndex ┬Ę DataSize(2) ┬Ę Data.
            let _logon = cur.take_u8()?;
            let req = RopWriteStreamRequest::decode_after_ropid(cur)?;
            // A write requires a read/write Stream handle (a read-only attachment
            // stream, or an unbound handle, yields `AccessDenied`). Writes only
            // land on body streams of a draft mail message; the bytes are staged
            // in the handle's buffer and flushed at `RopSaveChangesMessage`.
            let (return_value, written) = sessions
                .with_session_mut(session_id, |s| {
                    let Some(Handle::Stream {
                        data,
                        cursor,
                        is_dirty,
                        read_only,
                        ..
                    }) = s.handle_mut(req.input_handle_index)
                    else {
                        return (RopErrorCode::NotFound, 0u16);
                    };
                    if *read_only {
                        return (RopErrorCode::AccessDenied, 0u16);
                    }
                    let buf = data.get_or_insert_with(Vec::new);
                    let start = usize::try_from(*cursor).unwrap_or(buf.len());
                    if start > buf.len() {
                        buf.resize(start, 0);
                    }
                    // Overwrite from the cursor, extending the buffer when the
                    // write runs past the current end.
                    let n = req.data.len();
                    if start + n > buf.len() {
                        buf.resize(start + n, 0);
                    }
                    buf[start..start + n].copy_from_slice(&req.data);
                    *cursor = u64::try_from(start + n).unwrap_or(u64::MAX);
                    *is_dirty = true;
                    (RopErrorCode::Success, u16::try_from(n).unwrap_or(u16::MAX))
                })
                .unwrap_or((RopErrorCode::NotFound, 0u16));
            RopWriteStreamSuccess {
                input_handle_index: req.input_handle_index,
                return_value,
                written_size: written,
            }
            .encode(out);
        }
        RopId::ROP_SEEK_STREAM => {
            // ┬¦2.2.9.8.1: LogonId ┬Ę InputHandleIndex ┬Ę Origin(1) ┬Ę Offset(8).
            let _logon = cur.take_u8()?;
            let req = RopSeekStreamRequest::decode_after_ropid(cur)?;
            // Resolve the new cursor against the current stream state.
            let (current, len) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Stream { data, cursor, .. } => {
                        let l = data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
                        (*cursor, l)
                    }
                    _ => (0u64, 0u64),
                })
                .unwrap_or((0, 0));
            let new_pos = match req.resolve(current, len) {
                Ok(p) => p,
                Err(e) => {
                    // Encode the ROP-level InvalidParameter once and return Ok so
                    // the outer Execute loop does not emit a SECOND error ROP
                    // (qodo #1, coderabbit critical): taking the remaining bytes
                    // keeps the chain cursor aligned for the following ROP.
                    let _ = cur.take_remaining();
                    RopErrorResponse {
                        rop_id,
                        output_handle_index: req.input_handle_index,
                        return_value: RopErrorCode::InvalidParameter,
                    }
                    .encode(out);
                    let _ = e;
                    return Ok(());
                }
            };
            sessions.with_session_mut(session_id, |s| {
                if let Some(Handle::Stream { cursor, .. }) = s.handle_mut(req.input_handle_index) {
                    *cursor = new_pos;
                }
            });
            RopSeekStreamSuccess {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::Success,
                new_position: new_pos,
            }
            .encode(out);
        }
        RopId::ROP_SET_STREAM_SIZE => {
            // 2.2.9.7.1: LogonId - InputHandleIndex - StreamSize(8).
            let _logon = cur.take_u8()?;
            let req = RopSetStreamSizeRequest::decode_after_ropid(cur)?;
            // Cap the growable buffer at the smaller of the spec ceiling (2^31)
            // and the configured `max_attachment_bytes`. A single SetStreamSize
            // could otherwise zero-fill up to 2 GiB per handle (256 handles per
            // session, unbounded sessions) with no accounting against the
            // config, an authenticated memory-exhaustion vector (coderabbit).
            let max = 0x8000_0000u64
                .min(u64::try_from(cfg.max_attachment_bytes()).unwrap_or(0x8000_0000));
            if req.stream_size > max {
                RopSetStreamSizeResponse {
                    input_handle_index: req.input_handle_index,
                    return_value: RopErrorCode::NotEnoughMemory,
                }
                .encode(out);
                return Ok(());
            }
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    let Some(Handle::Stream {
                        data,
                        cursor,
                        is_dirty,
                        read_only,
                        ..
                    }) = s.handle_mut(req.input_handle_index)
                    else {
                        return RopErrorCode::NotFound;
                    };
                    if *read_only {
                        return RopErrorCode::AccessDenied;
                    }
                    let buf = data.get_or_insert_with(Vec::new);
                    let new_len = usize::try_from(req.stream_size).unwrap_or(buf.len());
                    if new_len <= buf.len() {
                        buf.truncate(new_len);
                    } else {
                        buf.resize(new_len, 0);
                    }
                    if *cursor > new_len as u64 {
                        *cursor = new_len as u64;
                    }
                    *is_dirty = true;
                    RopErrorCode::Success
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopSetStreamSizeResponse {
                input_handle_index: req.input_handle_index,
                return_value,
            }
            .encode(out);
        }
        RopId::ROP_GET_STREAM_SIZE => {
            // 2.2.9.6.1: LogonId - InputHandleIndex.
            let _logon = cur.take_u8()?;
            let req = RopGetStreamSizeRequest::decode_after_ropid(cur)?;
            // Resolve the buffered length plus the attachment's known/dirty state.
            // `found` distinguishes a real (bound) stream from an unbound or
            // non-stream handle: the latter must NOT be reported as a successful
            // zero-length stream, but as `InvalidParameter` to match the other
            // stream ROPs (sourcery). For an attachment stream whose blob has not
            // been downloaded yet, surface the declared `known_len` (captured at
            // OpenStream from `attachments[].size`) instead of 0, so the client
            // does not treat the stream as empty before the first ReadStream.
            let (len, known, found) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Stream {
                        data, known_len, ..
                    } => {
                        let l = data.as_ref().map(|d| d.len() as u64).unwrap_or(0);
                        (l, known_len.unwrap_or(0), true)
                    }
                    _ => (0u64, 0u64, false),
                })
                .unwrap_or((0, 0, false));
            if !found {
                RopErrorResponse {
                    rop_id,
                    output_handle_index: req.input_handle_index,
                    return_value: RopErrorCode::InvalidParameter,
                }
                .encode(out);
                return Ok(());
            }
            let size = if len == 0 {
                u32::try_from(known).unwrap_or(0)
            } else {
                u32::try_from(len).unwrap_or(u32::MAX)
            };
            RopGetStreamSizeSuccess {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::Success,
                stream_size: size,
            }
            .encode(out);
        }
        RopId::ROP_COMMIT_STREAM => {
            // ┬¦2.2.9.5.1: LogonId ┬Ę InputHandleIndex.
            let _logon = cur.take_u8()?;
            let req = RopCommitStreamRequest::decode_after_ropid(cur)?;
            // JMAP persists at SaveChangesMessage time; CommitStream is a
            // successful no-op acknowledged so the client proceeds to
            // SaveChanges. An unbound handle is NotFound.
            let return_value = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Stream { .. } => RopErrorCode::Success,
                    _ => RopErrorCode::NotFound,
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopCommitStreamResponse {
                input_handle_index: req.input_handle_index,
                return_value,
            }
            .encode(out);
        }
        RopId::ROP_RESET_TABLE => {
            // ┬¦2.2.5.7.1: LogonId + InputHandleIndex. Reset the cursor to
            // the first row, drop any applied restriction / sort order, and
            // reset bookmarks. Outlook issues this when re-querying a table
            // after a refresh.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _req = RopResetTableRequest::decode(cur)?;
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table {
                        cursor,
                        restriction,
                        sort_orders,
                        next_bookmark,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    {
                        *cursor = 0;
                        *restriction = SRestriction::default();
                        sort_orders.clear();
                        *next_bookmark = 0;
                        RopErrorCode::Success
                    } else {
                        RopErrorCode::NotFound
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopResetTableResponse {
                input_handle_index,
                return_value,
            }
            .encode(out);
        }
        RopId::ROP_RESTRICT => {
            // ┬¦2.2.5.3.1: LogonId + InputHandleIndex + RestrictFlags(1) +
            // RestrictionDataSize(2) + RestrictionData. Apply the restriction
            // to the table; the next QueryRows materialises only matching
            // rows. RestrictFlags bit 0x01 = PRIOR_RESTRICTION: AND the new
            // restriction with the prior one rather than replacing it.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopRestrictRequest::decode(cur)?;
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table {
                        restriction,
                        cursor,
                        total,
                        rows,
                        column_set,
                        kind,
                        parent_backend_id,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    {
                        // PRIOR_RESTRICTION (0x01) AND-combines with the
                        // active restriction; otherwise replace it.
                        *restriction = if req.restrict_flags & 0x01 != 0 {
                            SRestriction::And(vec![restriction.clone(), req.restriction.clone()])
                        } else {
                            req.restriction.clone()
                        };
                        *cursor = 0;
                        // ONE pass: derive the filtered `total` from the
                        // SAME `filtered_indices` builder `RopQueryRows` uses,
                        // so `RopQueryPosition`'s denominator always equals
                        // the rows QueryRows would serve (a Restrict issued
                        // before SetColumns resolves via the union of the
                        // column set and the restriction's referenced tags).
                        let cs = column_set.clone();
                        let pk = *kind;
                        let mb = parent_backend_id.clone();
                        let r = restriction.clone();
                        *total = filtered_indices(rows, &cs, &r, pk, &mb).len() as u64;
                        RopErrorCode::Success
                    } else {
                        RopErrorCode::NotFound
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            let _ = req.restrict_flags;
            RopRestrictResponse {
                input_handle_index,
                return_value,
                table_status: 0,
            }
            .encode(out);
        }
        RopId::ROP_SORT_TABLE => {
            // ┬¦2.2.5.2.1: LogonId + InputHandleIndex + SortFlags(1) +
            // SortOrder array. Materialise cells for each row once, then
            // stable-sort the row buffer in place. The cursor is preserved
            // (sorting reorders the buffer; Outlook re-queries afterwards).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSortTableRequest::decode(cur)?;
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table {
                        rows,
                        column_set,
                        sort_orders,
                        kind,
                        parent_backend_id,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    {
                        *sort_orders = req.sort_orders.clone();
                        let cs = column_set.clone();
                        let pk = *kind;
                        let mb = parent_backend_id.clone();
                        sort_rows(rows, &req.sort_orders, &cs, pk, &mb);
                        RopErrorCode::Success
                    } else {
                        RopErrorCode::NotFound
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopSortTableResponse {
                input_handle_index,
                return_value,
                table_status: 0,
            }
            .encode(out);
        }
        RopId::ROP_QUERY_POSITION => {
            // ┬¦2.2.7.1.1: LogonId + InputHandleIndex. Return the fractional
            // position of the cursor in the (post-restrict) row set.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _req = RopQueryPositionRequest::decode(cur)?;
            let (rv, num, den) = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table { cursor, total, .. }) =
                        s.handle_mut(input_handle_index)
                    {
                        let den = (*total).max(1) as u32;
                        let num = (*cursor as u32).min(den);
                        (RopErrorCode::Success, num, den)
                    } else {
                        (RopErrorCode::NotFound, 0u32, 1u32)
                    }
                })
                .unwrap_or((RopErrorCode::NotFound, 0u32, 1u32));
            RopQueryPositionResponse {
                input_handle_index,
                return_value: rv,
                numerator: num,
                denominator: den,
            }
            .encode(out);
        }
        RopId::ROP_SEEK_ROW => {
            // ┬¦2.2.7.2.1: LogonId + InputHandleIndex + SeekFlags(1)
            // + RowCount(4 LE signed). Move the cursor by RowCount rows from
            // the current position (default) or from the beginning when
            // SeekFlags bit 0x01 is set. Clamp at the table bounds.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSeekRowRequest::decode(cur)?;
            let (rv, has_sought_less) = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table { cursor, total, .. }) =
                        s.handle_mut(input_handle_index)
                    {
                        let len = *total as usize;
                        let from_begin = req.seek_flags & 0x01 != 0;
                        if from_begin {
                            *cursor = 0;
                        }
                        let target = if req.row_count < 0 {
                            let back = (req.row_count.unsigned_abs() as usize).min(*cursor);
                            *cursor -= back;
                            req.row_count.unsigned_abs() as i32 - back as i32
                        } else {
                            let fwd = (req.row_count as usize).min(len.saturating_sub(*cursor));
                            *cursor += fwd;
                            req.row_count - fwd as i32
                        };
                        // `target` is the unsatisfied remainder; clamping
                        // happened iff that remainder is non-zero. This also
                        // repairs the backward-seek case: a fully-satisfied
                        // negative seek yields target=0 (no clamping) where the
                        // previous `(target != req.row_count)` test always
                        // compared a non-negative remainder against a
                        // negative request and spuriously reported clamping.
                        let sought_less = (target != 0) as u8;
                        (RopErrorCode::Success, sought_less)
                    } else {
                        (RopErrorCode::NotFound, 0u8)
                    }
                })
                .unwrap_or((RopErrorCode::NotFound, 0u8));
            RopSeekRowResponse {
                input_handle_index,
                return_value: rv,
                has_sought_less,
            }
            .encode(out);
        }
        RopId::ROP_SEEK_ROW_BOOKMARK => {
            // ┬¦2.2.7.3.1: LogonId + InputHandleIndex + SeekFlags(1)
            // + Bookmark(4 LE) + RowCount(4 LE signed). CreateBookmark pins
            // the bookmark to the row's stable `row_id` (a 4-byte ULONG per
            // MS-OXCTABL), so the bookmark survives a RopSortTable reorder:
            // resolve it by scanning the rows for the matching row_id, then
            // move the cursor by RowCount from that origin, clamped to bounds.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSeekRowBookmarkRequest::decode(cur)?;
            let (rv, rows_sought, has_sought_less) = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table {
                        cursor,
                        total,
                        rows,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    {
                        let len = *total as usize;
                        // Resolve the bookmark to a row index by matching the
                        // stored row_id; fall back to clamping the raw bookmark
                        // value into bounds (defensive ŌĆö the spec pins it to a
                        // real bookmark but a stale/garbage value must not
                        // panic).
                        let origin = rows
                            .iter()
                            .position(|r| u64::from(req.bookmark) == r.row_id)
                            .unwrap_or_else(|| (req.bookmark as usize).min(len));
                        let moved;
                        let target = if req.row_count < 0 {
                            let back = (req.row_count.unsigned_abs() as usize).min(origin);
                            *cursor = origin - back;
                            moved = -(back as i32);
                            req.row_count.unsigned_abs() as i32 - back as i32
                        } else {
                            let fwd = (req.row_count as usize).min(len.saturating_sub(origin));
                            *cursor = origin + fwd;
                            moved = fwd as i32;
                            req.row_count - fwd as i32
                        };
                        // rows_sought is the signed number of rows ACTUALLY
                        // moved (not the unsatisfied remainder, which the
                        // previous code returned ŌĆö that reported 0 for a
                        // successful seek and the full request for a clamped
                        // one, inverting the field's documented semantics).
                        // has_sought_less is set from the non-zero remainder.
                        (RopErrorCode::Success, moved, (target != 0) as u8)
                    } else {
                        (RopErrorCode::NotFound, 0i32, 0u8)
                    }
                })
                .unwrap_or((RopErrorCode::NotFound, 0i32, 0u8));
            RopSeekRowBookmarkResponse {
                input_handle_index,
                return_value: rv,
                rows_sought,
                has_sought_less,
            }
            .encode(out);
        }
        RopId::ROP_SEEK_ROW_FRACTIONAL => {
            // ┬¦2.2.7.4.1: LogonId + InputHandleIndex + Numerator/Denominator
            // (4 LE each). Move the cursor to floor(Numerator/Denominator *
            // total). Clamp to [0,total].
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSeekRowFractionalRequest::decode(cur)?;
            let rv = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table { cursor, total, .. }) =
                        s.handle_mut(input_handle_index)
                    {
                        let len = *total as usize;
                        let pos = if req.denominator == 0 {
                            0
                        } else {
                            ((u64::from(req.numerator) * len as u64) / u64::from(req.denominator))
                                as usize
                        };
                        *cursor = pos.min(len);
                        RopErrorCode::Success
                    } else {
                        RopErrorCode::NotFound
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopSeekRowFractionalResponse {
                input_handle_index,
                return_value: rv,
            }
            .encode(out);
        }
        RopId::ROP_CREATE_BOOKMARK => {
            // ┬¦2.2.7.5.1: 4-byte header (RopId + LogonId + InputHandleIndex +
            // OutputHandleIndex); no body. Return a 4-byte MAPI Bookmark
            // (a ULONG per MS-OXCTABL) pinned to the row at the current
            // cursor's STABLE row_id ŌĆö NOT the absolute index ŌĆö so the
            // bookmark survives a subsequent RopSortTable reorder
            // (RopSeekRowBookmark resolves it by scanning rows for that
            // row_id). FreeBookmark is a stateless ack (the bookmark carries
            // no server-side resource).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?; // OutputHandleIndex
            let _ = RopCreateBookmarkRequest::decode(cur)?;
            let (output_handle_index, bookmark, rv) = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::Table {
                        cursor,
                        rows,
                        total,
                        ..
                    }) = s.handle_mut(input_handle_index)
                    {
                        let pos = *cursor;
                        // Pin the bookmark to the current row's stable
                        // row_id (clamped into the live row set); if the
                        // cursor is at EOF cap to the last row's id, or 0
                        // for an empty table.
                        let row_id = if pos < rows.len() {
                            rows[pos].row_id
                        } else if !rows.is_empty() {
                            rows[rows.len() - 1].row_id
                        } else {
                            0
                        };
                        // row_id is a FNV-1a 64-bit with the low bit
                        // reserved; truncate to the low 32 bits the wire
                        // carries so SeekRowBookmark's u32 round-trips it.
                        let bm = u32::try_from(row_id & 0xFFFF_FFFF).unwrap_or(0);
                        let _ = total; // total drives no bookmark state
                        (output_handle_index, bm, RopErrorCode::Success)
                    } else {
                        (output_handle_index, 0u32, RopErrorCode::NotFound)
                    }
                })
                .unwrap_or((output_handle_index, 0u32, RopErrorCode::NotFound));
            RopCreateBookmarkResponse {
                output_handle_index,
                return_value: rv,
                bookmark,
            }
            .encode(out);
        }
        RopId::ROP_FREE_BOOKMARK => {
            // ┬¦2.2.7.6.1: LogonId + InputHandleIndex + Bookmark(4 LE).
            // Bookmarks here are stateless (the bookmark encodes a stable
            // row_id, no server-side resource), so this is a successful no-op.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopFreeBookmarkRequest::decode(cur)?;
            let _ = req.bookmark;
            let return_value = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Table { .. } => RopErrorCode::Success,
                    _ => {
                        let _ = req.bookmark;
                        RopErrorCode::NotFound
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopFreeBookmarkResponse {
                input_handle_index,
                return_value,
            }
            .encode(out);
        }
        RopId::ROP_FAST_TRANSFER_SOURCE_COPY_MESSAGES => {
            // MS-OXCFXICS ┬¦3.1.1.1: RopId + LogonId + InputHandleIndex +
            // OutputHandleIndex + Flags(1) + MessageIdCount(2 LE) +
            // MessageIds[]. The input handle is a contents/folder table; we
            // serialise its cached rows as an incremental-change ICS stream
            // installed under the output handle. A non-empty `message_ids`
            // selects only those messages (matched by `row_id`, which is the
            // stable message Mid); an empty list means "all messages in the
            // view" per MS-OXCFXICS, so we do NOT truncate.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopFastTransferSourceCopyMessagesRequest::decode(cur)?;
            let message_ids = req.message_ids.clone();
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    fasttransfer_source_from_input(
                        s,
                        input_handle_index,
                        |s, rows, cs, kind, mb| {
                            let stream = if message_ids.is_empty() {
                                build_ics_stream(rows, &cs, kind, mb)
                            } else {
                                let selection: Vec<&crate::mapi::session::TableRow> = rows
                                    .iter()
                                    .filter(|r| message_ids.contains(&r.row_id))
                                    .collect();
                                build_ics_stream_sel(&selection, &cs, kind, mb)
                            };
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferSource {
                                    buffer: stream,
                                    cursor: 0,
                                    done: false,
                                },
                            );
                            RopErrorCode::Success
                        },
                    )
                })
                .unwrap_or(RopErrorCode::NotFound);
            let _ = req;
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_FAST_TRANSFER_SOURCE_COPY_MESSAGES);
        }
        RopId::ROP_FAST_TRANSFER_SOURCE_COPY_FOLDER => {
            // ┬¦3.1.1.2: 4-byte header + Flags(1). Serialise the whole subfolder
            // (hierarchy + contents) from the input folder's contents/hierarchy
            // table. A `Handle::Folder` input (no cached table rows) is
            // rejected with the explicit `NoSupport` so the failure mode is
            // distinguishable from a missing handle ŌĆö Outlook must open a
            // GetContentsTable/GetHierarchyTable first.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopFastTransferSourceCopyFolderRequest::decode(cur)?;
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    fasttransfer_source_from_input(
                        s,
                        input_handle_index,
                        |s, rows, cs, kind, mb| {
                            let stream = build_ics_stream(rows, &cs, kind, mb);
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferSource {
                                    buffer: stream,
                                    cursor: 0,
                                    done: false,
                                },
                            );
                            RopErrorCode::Success
                        },
                    )
                })
                .unwrap_or(RopErrorCode::NotFound);
            let _ = req;
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_FAST_TRANSFER_SOURCE_COPY_FOLDER);
        }
        RopId::ROP_FAST_TRANSFER_SOURCE_COPY_TO => {
            // ┬¦3.1.1.3: 4-byte header + Flags(1) + CopyToFlags(1) +
            // PropertyTagCount(2) + Tags[]. Property-only transfer keyed on
            // the requested tags (intersected with the table's column set).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopFastTransferSourceCopyToRequest::decode(cur)?;
            let want: Vec<u16> = req.property_tags.iter().map(|t| t.property_id).collect();
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    fasttransfer_source_from_input(
                        s,
                        input_handle_index,
                        |s, rows, cs, kind, mb| {
                            let filtered: Vec<crate::mapi::data::PropertyTag> = cs
                                .iter()
                                .filter(|t| want.contains(&t.property_id))
                                .copied()
                                .collect();
                            let stream = build_ics_stream(rows, &filtered, kind, mb);
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferSource {
                                    buffer: stream,
                                    cursor: 0,
                                    done: false,
                                },
                            );
                            RopErrorCode::Success
                        },
                    )
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_FAST_TRANSFER_SOURCE_COPY_TO);
        }
        RopId::ROP_FAST_TRANSFER_SOURCE_COPY_PROPERTIES => {
            // 0x69: 4-byte header + Flags(1) + TagCount(2) + Tags[]. Like
            // CopyTo but the spec'd tag set is the whole-message property set
            // (intersected with the table's column set).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopFastTransferSourceCopyPropertiesRequest::decode(cur)?;
            let want: Vec<u16> = req.property_tags.iter().map(|t| t.property_id).collect();
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    fasttransfer_source_from_input(
                        s,
                        input_handle_index,
                        |s, rows, cs, kind, mb| {
                            let filtered: Vec<crate::mapi::data::PropertyTag> = cs
                                .iter()
                                .filter(|t| want.contains(&t.property_id))
                                .copied()
                                .collect();
                            let stream = build_ics_stream(rows, &filtered, kind, mb);
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferSource {
                                    buffer: stream,
                                    cursor: 0,
                                    done: false,
                                },
                            );
                            RopErrorCode::Success
                        },
                    )
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_FAST_TRANSFER_SOURCE_COPY_PROPERTIES);
        }
        RopId::ROP_FAST_TRANSFER_SOURCE_GET_BUFFER => {
            // ┬¦3.1.1.5.1: LogonId + InputHandleIndex + BufferSize(2 LE) +
            // TransferFlags(1). Serve the next chunk from the source handle;
            // transition to `Done` when the buffer is exhausted. The response
            // carries the success-body ONLY when ReturnValue==Success; a
            // non-success ROP emits the bare header (handled in the encoder).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopFastTransferSourceGetBufferRequest::decode(cur)?;
            let max = usize::from(req.buffer_size).max(1);
            let resp = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::FastTransferSource {
                        buffer,
                        cursor,
                        done,
                    }) = s.handle_mut(input_handle_index)
                    {
                        if *done {
                            return RopFastTransferSourceGetBufferSuccess {
                                input_handle_index,
                                return_value: RopErrorCode::Success,
                                transfer_status: 3, // Done
                                in_progress_count: 0,
                                total_step_count: 0,
                                transfer_buffer_size: 0,
                                data: Vec::new(),
                            };
                        }
                        let remaining = buffer.len() - (*cursor).min(buffer.len());
                        let take = remaining.min(max);
                        let data: Vec<u8> = buffer[*cursor..*cursor + take].to_vec();
                        let total = buffer.len();
                        *cursor += take;
                        let transfer_status = if *cursor >= buffer.len() {
                            *done = true;
                            3u16 // Done
                        } else {
                            1u16 // InProgress/Partial
                        };
                        RopFastTransferSourceGetBufferSuccess {
                            input_handle_index,
                            return_value: RopErrorCode::Success,
                            transfer_status,
                            in_progress_count: u16::try_from(*cursor).unwrap_or(u16::MAX),
                            total_step_count: u16::try_from(total).unwrap_or(u16::MAX),
                            transfer_buffer_size: u16::try_from(take).unwrap_or(u16::MAX),
                            data,
                        }
                    } else {
                        RopFastTransferSourceGetBufferSuccess {
                            input_handle_index,
                            return_value: RopErrorCode::NotFound,
                            transfer_status: 0, // Error
                            in_progress_count: 0,
                            total_step_count: 0,
                            transfer_buffer_size: 0,
                            data: Vec::new(),
                        }
                    }
                })
                .unwrap_or(RopFastTransferSourceGetBufferSuccess {
                    input_handle_index,
                    return_value: RopErrorCode::NotFound,
                    transfer_status: 0,
                    in_progress_count: 0,
                    total_step_count: 0,
                    transfer_buffer_size: 0,
                    data: Vec::new(),
                });
            resp.encode(out);
        }
        RopId::ROP_FAST_TRANSFER_DESTINATION_CONFIGURE => {
            // ┬¦3.1.2.1: 4-byte header + SourceFmt(1) + SyncFlags(1). Install a
            // destination handle carrying the parent folder's backend id (lifted
            // from the input folder handle) so subsequent PutBuffer apply steps
            // know where message changes land.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopFastTransferDestinationConfigureRequest::decode_after_ropid(cur)?;
            // Only accept a Folder or Table input handle (the destination must
            // land real message changes somewhere); a wrong/absent input
            // returns NotFound rather than installing a destination that can
            // never apply (the prior `unwrap_or_default()` installed an empty
            // parent and acked Success).
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    let parent = s.handle_mut(input_handle_index).and_then(|h| match h {
                        Handle::Folder { backend_id, .. } => Some(backend_id.clone()),
                        Handle::Table {
                            parent_backend_id, ..
                        } => Some(parent_backend_id.clone()),
                        _ => None,
                    });
                    match parent {
                        Some(parent) => {
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferDestination {
                                    buffer: Vec::new(),
                                    source_fmt: req.source_fmt,
                                    parent_backend_id: parent,
                                    finalised: false,
                                },
                            );
                            RopErrorCode::Success
                        }
                        None => RopErrorCode::NotFound,
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_FAST_TRANSFER_DESTINATION_CONFIGURE);
        }
        RopId::ROP_FAST_TRANSFER_DESTINATION_PUT_BUFFER => {
            // ┬¦3.1.2.2: LogonId + InputHandleIndex + DataSize(2 LE) + Data.
            // Append the bytes to the destination's staging buffer (capped by
            // the configured `max_attachment_bytes` so a client cannot drive
            // an unbounded across-request accumulation to OOM). A zero-length
            // PutBuffer signals end-of-stream; the gateway then tokenises the
            // accumulated buffer and applies the deltas. Tokenisation runs
            // OUTSIDE the session write lock (the buffer is cloned out first)
            // so a long FXICS pass never blocks other sessions, and the apply
            // now `?`-propagates a typed DecodeError as `DiskError` (fail
            // closed) rather than swallowing a malformed stream as Success.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopFastTransferDestinationPutBufferRequest::decode_after_ropid(cur)?;
            // Decide under the lock: cap + append, or finalise-and-extract.
            let cap = cfg.max_attachment_bytes();
            let decision = sessions
                .with_session_mut(session_id, |s| {
                    if let Some(Handle::FastTransferDestination {
                        buffer,
                        finalised,
                        parent_backend_id,
                        source_fmt,
                    }) = s.handle_mut(input_handle_index)
                    {
                        if *finalised {
                            return Ok(None);
                        }
                        if req.data.is_empty() {
                            // End-of-stream: clone the accumulated bytes out so
                            // the tokenizer run happens after the lock drops.
                            *finalised = true;
                            Ok(Some((
                                std::mem::take(buffer),
                                *source_fmt,
                                parent_backend_id.clone(),
                            )))
                        } else {
                            if buffer.len().saturating_add(req.data.len()) > cap {
                                return Err(RopErrorCode::NotEnoughMemory);
                            }
                            buffer.extend_from_slice(&req.data);
                            Ok(None)
                        }
                    } else {
                        Err(RopErrorCode::NotFound)
                    }
                })
                .unwrap_or(Err(RopErrorCode::NotFound));
            match decision {
                Err(rv) => {
                    RopFastTransferDestinationPutBufferResponse {
                        input_handle_index,
                        return_value: rv,
                        transfer_status: 2,
                        data_remaining: 0,
                    }
                    .encode(out);
                }
                Ok(Some((buffer, source_fmt, parent_backend_id))) => {
                    // Apply OUTSIDE the lock. A malformed upload surfaces as
                    // `DiskError` (fail closed) per the apply contract; each
                    // event is best-effort (logged) so a single untranslatable
                    // item never aborts the rest of the upload.
                    let apply_rv = match apply_fasttransfer_upload(
                        jmap,
                        password,
                        username,
                        &buffer,
                        source_fmt,
                        &parent_backend_id,
                    )
                    .await
                    {
                        Ok(()) => RopErrorCode::Success,
                        Err(_) => RopErrorCode::DiskError,
                    };
                    RopFastTransferDestinationPutBufferResponse {
                        input_handle_index,
                        return_value: apply_rv,
                        transfer_status: 1,
                        data_remaining: 0,
                    }
                    .encode(out);
                }
                Ok(None) => {
                    RopFastTransferDestinationPutBufferResponse {
                        input_handle_index,
                        return_value: RopErrorCode::Success,
                        transfer_status: 1,
                        data_remaining: 0,
                    }
                    .encode(out);
                }
            }
        }
        RopId::ROP_SYNCHRONIZATION_CONFIGURE => {
            // ┬¦3.3.1.1: 4-byte header + SyncFlags(1) + SyncType(1) +
            // StateLen(2 LE) + State(...). Install a destination/source handle
            // echoing the supplied sync state so the client's next GetBuffer /
            // PutBuffer round-trip is bound to the configured context. Only a
            // Folder or Table input is accepted (the sync must target a real
            // collection); the supplied `sync_state` is seeded onto the
            // destination buffer and `sync_type` onto `source_fmt` so the apply
            // step receives both (the prior code discarded sync_type/state and
            // installed a destination against an empty parent even for an
            // unresolvable input handle).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let output_handle_index = cur.take_u8()?;
            let req = RopSynchronizationConfigureRequest::decode_after_ropid(cur)?;
            let return_value = sessions
                .with_session_mut(session_id, |s| {
                    let parent = s.handle_mut(input_handle_index).and_then(|h| match h {
                        Handle::Folder { backend_id, .. } => Some(backend_id.clone()),
                        Handle::Table {
                            parent_backend_id, ..
                        } => Some(parent_backend_id.clone()),
                        _ => None,
                    });
                    match parent {
                        Some(parent) => {
                            s.set_handle(
                                output_handle_index,
                                Handle::FastTransferDestination {
                                    // Seed the configured sync state so the
                                    // apply pass starts from the client's
                                    // anchor rather than an empty buffer.
                                    buffer: req.sync_state.clone(),
                                    source_fmt: req.sync_type,
                                    parent_backend_id: parent,
                                    finalised: false,
                                },
                            );
                            RopErrorCode::Success
                        }
                        None => RopErrorCode::NotFound,
                    }
                })
                .unwrap_or(RopErrorCode::NotFound);
            RopFastTransferSourceOpenResponse {
                output_handle_index,
                return_value,
            }
            .encode(out, RopId::ROP_SYNCHRONIZATION_CONFIGURE);
        }
        RopId::ROP_SYNCHRONIZATION_IMPORT_MESSAGE_CHANGE
        | RopId::ROP_SYNCHRONIZATION_IMPORT_HIERARCHY_CHANGE
        | RopId::ROP_SYNCHRONIZATION_IMPORT_DELETES
        | RopId::ROP_SYNCHRONIZATION_IMPORT_MESSAGE_MOVE
        | RopId::ROP_SYNCHRONIZATION_IMPORT_READ_STATE_CHANGES
        | RopId::ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_BEGIN
        | RopId::ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_CONTINUE
        | RopId::ROP_SYNCHRONIZATION_UPLOAD_STATE_STREAM_END => {
            // ┬¦3.3.2.x: the import/upload-state ROPs are server-side applies
            // the client sends inside a FastTransfer destination context. The
            // gateway has already accumulated the upload stream via Destination
            // PutBuffer, so these per-ROP envelopes are best-effort
            // acknowledgements that route their body onto the destination
            // handle's staging buffer.
            //
            // Body bound: these import/upload-state ROPs are TERMINAL ŌĆö
            // Outlook emits one import ROP as the final element of a
            // FastTransfer sub-operation, never coalesced with a following
            // ROP in the same Execute buffer (the bulk message content
            // arrives over a separate PutBuffer stream, NOT inline here).
            // Their bodies are `PropertyValueCount`-prefixed variable arrays
            // (e.g. ImportMessageChange = ImportFlag(1) + PropertyValueCount(2)
            // + PropertyValues[], per MS-OXCFXICS ┬¦2.2.3.2.4.2.1) with no
            // per-ROP trailing-size field, so `take_remaining` is the only
            // cursor-alignment-preserving consume. We bound the consumed body
            // against the configured upload ceiling (mirroring the
            // destination buffer cap) so a coalesced malicious tail cannot
            // drive an unbounded staging extension, and the imported count
            // is logged for diagnostics.
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            // Consume the whole terminal body so the chain cursor is empty
            // after this ROP (the import ROP is the last element). Stage up to
            // the configured upload cap; an oversized body is dropped with a
            // warn (fail open at the staging layer ŌĆö the JMAP apply is what
            // rejects the actual delta ŌĆö but the ROP chain stays aligned).
            let body = cur.take_remaining().to_vec();
            let cap = cfg.max_attachment_bytes();
            let imported = body.len();
            let _ = sessions.with_session_mut(session_id, |s| {
                if let Some(Handle::FastTransferDestination { buffer, .. }) =
                    s.handle_mut(input_handle_index)
                {
                    if body.len() <= cap {
                        buffer.extend_from_slice(&body);
                    } else {
                        tracing::warn!(
                            cap,
                            body_len = body.len(),
                            "sync import body exceeds upload cap; dropped from staging"
                        );
                    }
                }
            });
            tracing::debug!(
                imported,
                "sync import/upload-state ROP body appended to destination"
            );
            RopSynchronizationAckResponse {
                input_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out, rop_id);
        }
        RopId::ROP_REGISTER_NOTIFICATION => {
            // MS-OXCROPS §2.2.14.1: 4-byte RopHeader4 then NotificationTypes(2
            // LE) · [Reserved(1) if Extended flag 0x0400 set] · WantWholeStore(1)
            // · [FolderId(8) · MessageId(8) if WantWholeStore==0]. The
            // dispatcher consumed the leading RopId, so read the trailing
            // 3-byte header via `decode_after_ropid` (the body-only convention;
            // `RopRegisterNotificationRequest::decode` re-takes the handle
            // indices and is unsuitable for the in-loop dispatcher).
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let notification_types = cur.take_u16_le()?;
            // Extended (0x0400) flag ⇒ a trailing Reserved byte follows.
            if notification_types & 0x0400 != 0 {
                let _reserved = cur.take_u8()?;
            }
            let want_whole_store = cur.take_u8()?;
            let (folder_id_raw, _message_id_raw) = if want_whole_store == 0 {
                (Some(cur.take_u64_le()?), Some(cur.take_u64_le()?))
            } else {
                (None, None)
            };
            // Resolve the folder scope: a whole-store registration needs no
            // folder id; a folder-scoped registration carries the raw MAPI row id
            // verbatim. The sink compares events against that row id by mapping
            // each event's backend folder id back to its row id via
            // `store::folder_id_from_backend`, so the subscription honours the
            // client's filter WITHOUT depending on an open `Handle::Folder` (the
            // client may release the folder before registering, or register
            // against a folder reached through a table row) and NEVER widens an
            // unresolvable scope to the whole store (which would push events for
            // folders the client never subscribed to).
            let scope = if want_whole_store != 0 || folder_id_raw.is_none() {
                NotificationScope::WholeStore
            } else {
                NotificationScope::Folder(folder_id_raw.unwrap_or(0))
            };
            // Subscribe a per-session sink keyed by the client's
            // OutputHandleIndex. When no shared SubscriptionManager is wired
            // (unit-test fixtures), emulate an empty-feed registration: install
            // the sink's metadata so `RopRelease` can find it, but with a
            // best-effort receiver. The arm echoes `Success` either way so the
            // client believes the registration took (it will simply never fire
            // in a fixture, exactly as the Phase-0 behaviour did).
            if let Some(mgr) = subscription_manager {
                let receiver = mgr.subscribe_raw();
                sessions.notifications().register(
                    *session_id,
                    h4.output_handle_index,
                    MapiNotificationSink::new(
                        username.to_string(),
                        notification_types,
                        scope,
                        h4.logon_id,
                        receiver,
                    ),
                );
            }
            // Echo the spec response (RopId · OutputHandleIndex ·
            // ReturnValue=Success).
            RopRegisterNotificationResponse {
                output_handle_index: h4.output_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        _ => {
            let _ = cur.take_remaining();
            RopErrorResponse {
                rop_id,
                output_handle_index: 0,
                return_value: RopErrorCode::NotFound,
            }
            .encode(out);
        }
    }
    let _ = logon_id;
    Ok(())
}

/// Map a [`crate::jmap::EmailSetOutcome`] to a MAPI ROP return value.
///
/// - A method-level `error` (e.g. `accountNotFound`, rate limit) or any per-id
///   rejection in `not_updated` is a server-side failure: report
///   [`RopErrorCode::DiskError`] so Outlook surfaces it rather than believing
///   the write applied. Previously the property-write arms called
///   `update_email`, which returned `Ok(())` ignoring `notUpdated`, and the
///   handler mapped every `Ok` to `Success` ŌĆö masking real failures as
///   success (qodo #3/#5, cubic #23). The `label` string is included in the
///   log so partial-failure traces name which ROP the update served.
fn outcome_to_code(outcome: crate::jmap::EmailSetOutcome, label: &'static str) -> RopErrorCode {
    if let Some(desc) = outcome.method_error {
        tracing::warn!(error = %desc, "%{label}: JMAP method rejected update");
        return RopErrorCode::DiskError;
    }
    if !outcome.not_updated.is_empty() {
        // Per-RFC-8621 ┬¦4.5 a `notUpdated` entry signals the server refused
        // that id; for the single-id patches the property-write arms send,
        // any rejection means the whole apply did not take, so report
        // DiskError rather than a misleading partial Success.
        tracing::warn!(
            failures = outcome.not_updated.len(),
            details = ?outcome.not_updated,
            "%{label}: JMAP Email/set rejected ids"
        );
        RopErrorCode::DiskError
    } else {
        RopErrorCode::Success
    }
}

/// Publish an `ItemModified` notification when a MAPI property write took
/// effect, so the MAPI session's `NotificationWait` long-poll (and the EWS
/// subscription feed that shares the same `SubscriptionManager`) sees the
/// change instead of forcing the client to re-poll (qodo #9, cubic #30, audit
/// ┬¦2e). No-op when the gateway was built without a manager wired (unit-test
/// fixtures pass `None`).
fn publish_item_modified(
    subscription_manager: Option<&std::sync::Arc<crate::notifications::SubscriptionManager>>,
    owner: &str,
    folder_id: &str,
    item_id: &str,
) {
    if let Some(mgr) = subscription_manager
        && !owner.is_empty()
        && !item_id.is_empty()
    {
        mgr.publish(crate::notifications::NotificationEvent::ItemModified {
            owner: owner.to_string(),
            folder_id: folder_id.to_string(),
            item_id: item_id.to_string(),
            change_key: String::new(),
        });
    }
}

/// Decode `RopOpenFolder` body (`FolderId(8) + OpenModeFlags(1)`) after the
/// 4-byte header has been consumed.
fn decode_open_folder_body(
    cur: &mut Buf<'_>,
) -> Result<crate::mapi::rops::RopOpenFolderRequest, DecodeError> {
    let input_handle_index = 0u8;
    let output_handle_index = 0u8;
    let folder_id = cur.take_u64_le()?;
    let open_mode_flags = cur.take_u8()?;
    Ok(crate::mapi::rops::RopOpenFolderRequest {
        input_handle_index,
        output_handle_index,
        folder_id,
        open_mode_flags,
    })
}

/// Resolve the JMAP mailbox id with `role == "drafts"` (RFC 8621 ┬¦5.1) for the
/// account, used when a `RopCreateMessage` handle did not carry a parent
/// mailbox id (the client opened the synthetic root). Falls back to the empty
/// string on failure ŌĆö the JMAP server will reject the create with no mailbox,
/// mapped upstream to a `DiskError`.
async fn resolve_drafts_mailbox(
    jc: &crate::jmap::JmapClient,
    account_id: &str,
    username: &str,
    password: &secrecy::SecretString,
) -> String {
    match jc
        .get_mailbox_ids_for_role(account_id, "drafts", username, password)
        .await
    {
        Ok(ids) => ids.first().cloned().unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// Download the bytes of a streamed attachment blob. `packed_backend` carries
/// `<emailId>\x1F<blobId>` (the encoding stashed at `RopOpenStream` time); the
/// helper splits it, looks up the account id, and calls `download_blob`.
/// Returns `None` on any failure so the caller falls back to an empty stream
/// rather than surfacing a transport-level error mid-chain. The previous unused
/// `_mailbox_id` parameter is dropped (coderabbit). The download is bounded at
/// OpenStream by `max_attachment_bytes` (against the JMAP-declared attachment
/// size) before this helper runs, so the buffer never exceeds the configured
/// ceiling for a real attachment.
async fn materialise_attachment_blob(
    jc: Option<&crate::jmap::JmapClient>,
    password: Option<&secrecy::SecretString>,
    username: &str,
    packed_backend: &str,
    _cfg: &crate::config::Config,
) -> Option<Vec<u8>> {
    let jc = jc?;
    let pw = password?;
    let (_email_id, blob_id) = packed_backend.split_once('\x1F')?;
    if blob_id.is_empty() {
        return None;
    }
    let account_id = jc.get_account_id(username, pw).await.ok()?;
    jc.download_blob(&account_id, blob_id, username, pw)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, blob_id = %blob_id, "JMAP blob download (ReadStream) failed");
        })
        .ok()
}

/// Collect the envelope recipient addresses (to/cc/bcc) for a JMAP email, used
/// by `RopSubmitMessage` / `RopTransportSend` to build the
/// `EmailSubmission/set` envelope `rcptTo`. Duplicates are deduplicated to
/// avoid a single recipient being listed twice in the envelope.
fn email_recipients(e: &crate::jmap::JmapEmail) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for addrs in [&e.to, &e.cc, &e.bcc].into_iter().flatten() {
        for a in addrs {
            if let Some(addr) = a.email.as_deref()
                && !addr.is_empty()
                && seen.insert(addr.to_string())
            {
                out.push(addr.to_string());
            }
        }
    }
    out
}

/// Enumerate the messages in a JMAP mailbox as `TableRow`s carrying a cached
/// `JmapEmail` for lazy cell materialisation by `RopQueryRows`.
///
/// Audit §2f.1: the contents-table builder was previously capped at a fixed
/// 200 rows regardless of folder size, so a large mailbox was silently
/// truncated and Outlook never saw the rest of the folder. A real Exchange
/// mailbox exposes the full contents set and lets `RopQueryRows` page it
/// client-side, so this now drains the folder in `Config::mapi_contents_page_size`
/// pages (using JMAP `Email/query` `calculateTotal=true`) up to the
/// `Config::mapi_max_contents_rows` hard ceiling that protects the gateway
/// against a pathological mailbox. The cursor in `Handle::Table` then
/// hands `RopQueryRows` whatever slice the client asked for, across the full
/// materialised set.
async fn fetch_email_rows(
    cfg: &Config,
    jc: &crate::jmap::JmapClient,
    username: &str,
    password: &secrecy::SecretString,
    mailbox_id: &str,
) -> Vec<crate::mapi::session::TableRow> {
    let account_id = match jc.get_account_id(username, password).await {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };
    // `mapi_contents_page_size` and `mapi_max_contents_rows` are both
    // validated > 0 / ≥ page size at startup, so the floor here is 1.
    let page_size = cfg.mapi_contents_page_size.max(1) as u64;
    let max_rows = cfg.mapi_max_contents_rows.max(1);

    let mut rows: Vec<crate::mapi::session::TableRow> = Vec::new();
    let mut position: u64 = 0;
    // Defensive bound on the number of JMAP round-trips so a server that
    // mis-reports `total` cannot pin the loop indefinitely; the ceiling is
    // reached first for any real folder.
    let max_pages = max_rows.div_ceil(page_size as usize).max(1);
    for _ in 0..max_pages {
        if rows.len() >= max_rows {
            break;
        }
        let remaining = max_rows - rows.len();
        let limit = page_size.min(remaining as u64);
        let params = crate::jmap::QueryEmailsParams {
            account_id: &account_id,
            filter: Some(serde_json::json!({"inMailbox": mailbox_id})),
            sort: None,
            position,
            limit,
            username,
            password,
        };
        let list = match jc.query_emails(params).await {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    mailbox_id = %mailbox_id,
                    position,
                    "JMAP Email/query (contents-table drain) failed; \
                     returning {} rows collected so far",
                    rows.len()
                );
                break;
            }
        };
        if list.emails.is_empty() {
            break;
        }
        let received = list.emails.len() as u64;
        for e in list.emails {
            if rows.len() >= max_rows {
                break;
            }
            let jid = e.id.clone().unwrap_or_default();
            let source: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(e);
            rows.push(crate::mapi::session::TableRow {
                row_id: store::message_id_from_jmap(&jid),
                cells: Vec::new(),
                source: Some(source),
            });
        }
        // If the server returned fewer ids than `limit` we have drained the
        // folder; otherwise advance the JMAP cursor and continue paging.
        if received < limit {
            break;
        }
        position += limit;
        // JMAP `total` is an authoritative folder count when the server
        // honours `calculateTotal`; use it to short-circuit once the
        // remainder is exhausted so we never overrun past the last id.
        if list.total > 0 && position >= list.total {
            break;
        }
    }
    rows
}

/// Enumerate CalDAV VEVENTs in the user's default calendar collection as
/// `TableRow`s carrying a cached `CalendarItem` for lazy
/// `IPM.Appointment` cell materialisation. The window is ±2 years centred on
/// "now" (±730 days, a ~4-year span) so Outlook's contents-table view shows
/// upcoming and recent appointments without pulling the entire history
/// (Outlook refines the visible window client-side via `RopRestrict`
/// time-range).
async fn fetch_calendar_rows(
    cfg: &Config,
    username: &str,
    password: &secrecy::SecretString,
    mailbox_id: &str,
) -> Vec<crate::mapi::session::TableRow> {
    let _ = mailbox_id; // calendar uses the per-user collection href, not the
    // JMAP mailbox id (this is the synthetic Calendar folder).
    if cfg.caldav_base.is_empty() {
        tracing::debug!("mapi calendar contents: no caldav_base configured");
        return Vec::new();
    }
    let caldav = match crate::caldav::CaldavClient::new(cfg) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("mapi calendar contents: caldav client build failed: {}", e);
            return Vec::new();
        }
    };
    let href = caldav.calendar_collection_href(username);
    let now = chrono::Utc::now();
    let start = now - chrono::Duration::days(730);
    let end = now + chrono::Duration::days(730);
    let raw = match caldav
        .query_events(
            &href,
            &start.format("%Y%m%dT%H%M%SZ").to_string(),
            &end.format("%Y%m%dT%H%M%SZ").to_string(),
            username,
            password.expose_secret(),
        )
        .await
    {
        Ok(x) => x,
        Err(e) => {
            tracing::warn!("mapi calendar contents: caldav query_events failed: {}", e);
            return Vec::new();
        }
    };
    parse_calendar_multistatus(&raw)
}

/// Enumerate CardDAV vCards in the user's addressbook as `TableRow`s
/// carrying the raw vCard text for lazy `IPM.Contact` cell materialisation.
async fn fetch_contact_rows(
    cfg: &Config,
    username: &str,
    password: &secrecy::SecretString,
    mailbox_id: &str,
) -> Vec<crate::mapi::session::TableRow> {
    let _ = mailbox_id; // contacts uses the per-user addressbook home, not the
    // synthetic Contacts folder id.
    if cfg.carddav_base.is_empty() && cfg.caldav_base.is_empty() {
        tracing::debug!("mapi contacts contents: no carddav_base configured");
        return Vec::new();
    }
    let carddav = if !cfg.carddav_base.is_empty() {
        match crate::carddav::CarddavClient::new(cfg) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("mapi contacts contents: carddav client build failed: {}", e);
                return Vec::new();
            }
        }
    } else {
        crate::carddav::CarddavClient::from_caldav_base(&cfg.caldav_base)
    };
    let (contacts, _token) = match carddav
        .list_contacts(username, password.expose_secret(), None)
        .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "mapi contacts contents: carddav list_contacts failed: {}",
                e
            );
            return Vec::new();
        }
    };
    contacts
        .into_iter()
        .map(|c| {
            let source: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                std::sync::Arc::new(c.vcard);
            crate::mapi::session::TableRow {
                row_id: store::message_id_from_jmap(&c.href),
                cells: Vec::new(),
                source: Some(source),
            }
        })
        .collect()
}

/// Build one of the gateway-owned virtual folder rows (Calendar / Contacts)
/// for the hierarchy table. A synthetic `JmapMailbox` carries the folder id,
/// display name, and the `__calendar__`/`__contacts__` role so
/// `mailbox_to_cells` renders the correct `PR_CONTAINER_CLASS` and the
/// contents-table open step resolves the folder kind.
fn synth_folder_row(backend_id: &str, name: &str, role: &str) -> crate::mapi::session::TableRow {
    let mbx = crate::jmap::JmapMailbox {
        id: Some(backend_id.to_string()),
        name: Some(name.to_string()),
        parent_id: Some("ROOT".to_string()),
        role: Some(role.to_string()),
        sort_order: None,
        total_emails: None,
        unread_emails: None,
        total_threads: None,
        unread_threads: None,
        is_subscribed: None,
    };
    let source: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(mbx);
    crate::mapi::session::TableRow {
        row_id: store::folder_id_from_backend(backend_id),
        cells: Vec::new(),
        source: Some(source),
    }
}

/// Parse a CalDAV `calendar-query` multistatus body into `TableRow`s whose
/// source is a `CalendarItem` built from each `<C:calendar-data>` iCalendar
/// blob (via `calendar::parse_ics_event`). The row id is the FNV-1a of the
/// iCalendar UID (stable across Outlook sessions).
fn parse_calendar_multistatus(xml: &str) -> Vec<crate::mapi::session::TableRow> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut in_calendar_data = false;
    let mut caldata_buf = String::new();
    let mut rows = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                in_calendar_data = true;
                caldata_buf.clear();
            }
            Ok(Event::Text(ref t)) if in_calendar_data => {
                if let Ok(ics) = t.decode() {
                    caldata_buf.push_str(&ics);
                }
            }
            Ok(Event::CData(ref t)) if in_calendar_data => {
                caldata_buf.push_str(&String::from_utf8_lossy(t.as_ref()));
            }
            Ok(Event::End(e)) if e.name().local_name().as_ref() == b"calendar-data" => {
                in_calendar_data = false;
                let ics = caldata_buf.trim();
                if !ics.is_empty()
                    && let Some(item) = crate::calendar::parse_ics_event(ics)
                {
                    let uid = item.uid.clone();
                    let source: std::sync::Arc<dyn std::any::Any + Send + Sync> =
                        std::sync::Arc::new(item);
                    rows.push(crate::mapi::session::TableRow {
                        row_id: store::message_id_from_jmap(&uid),
                        cells: Vec::new(),
                        source: Some(source),
                    });
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                tracing::warn!("mapi calendar contents: multistatus parse error: {}", e);
                break;
            }
            _ => {}
        }
        // Reuse the scratch buffer across iterations but shrink it back so
        // it never retains memory proportional to the largest event in the
        // multistatus response (matches the established quick-xml pattern in
        // `src/caldav.rs`).
        buf.clear();
    }
    rows
}

/// Map a folder backend id back to a FolderKind using the live session
/// snapshot. For the synthetic "ROOT" id we return Root; otherwise we
/// consult the existing handles for a Folder with that backend id.
fn folder_kind_for_backend(
    backend_id: &str,
    snap: &crate::mapi::session::SessionSnapshot,
) -> FolderKind {
    if backend_id == "ROOT" {
        return FolderKind::Root;
    }
    // Synthetic calendar/contacts folder ids are gateway-defined, so resolve
    // them from the store map before walking the live handle table.
    if let Some(k) = store::folder_kind_for_backend_id(backend_id) {
        return k;
    }
    for h in snap.handles.values() {
        if let Handle::Folder {
            backend_id: bid,
            kind,
        } = h
            && bid == backend_id
        {
            return *kind;
        }
    }
    // Unknown folder ids default to Mail; the contents-table probe will
    // surface NoSupport if the backend can't serve them.
    FolderKind::Mail
}

/// Encode a row cell: emit the materialised `PropertyValue` when present and
/// type-compatible with the requested column, otherwise a typed NULL of the
/// column's declared type so the row decoder skips exactly the right byte
/// length per MS-OXCDATA s2.11.2. For `PR_FOLDER_ID`/`PR_MID`/
/// `PR_PARENT_FOLDER_ID` (Integer64) the cell falls back to the row id so the
/// client always gets a stable identity even when the backend didn't supply it.
fn encode_cell_for_row(
    out: &mut Vec<u8>,
    tag: &crate::mapi::data::PropertyTag,
    cell: crate::mapi::data::PropertyValue,
    row: &crate::mapi::session::TableRow,
) {
    use crate::mapi::data::PropertyValue;
    if !matches!(cell, PropertyValue::Null) {
        cell.encode(out);
        return;
    }
    encode_typed_null(out, tag, row);
}

/// Emit a typed NULL for a column whose backend converter returned `Null`
/// (the property is unknown / unsupported for this object) ŌĆö delegates to the
/// shared `store::typed_null_for_tag` so the wire shape stays consistent
/// across the QueryRows / GetProperties paths. For `PR_FOLDER_ID`/`PR_MID` /
/// `PR_PARENT_FOLDER_ID` the cell is overridden to the row id so the client
/// always gets a stable identity even when the backend didn't supply it.
fn encode_typed_null(
    out: &mut Vec<u8>,
    tag: &crate::mapi::data::PropertyTag,
    row: &crate::mapi::session::TableRow,
) {
    let mut v = store::typed_null_for_tag(tag);
    if tag.property_type == crate::mapi::data::PropertyType::PTYP_INTEGER64
        && matches!(
            tag.property_id,
            crate::mapi::store::PR_FOLDER_ID
                | crate::mapi::store::PR_MID
                | crate::mapi::store::PR_PARENT_FOLDER_ID
        )
    {
        v = crate::mapi::data::PropertyValue::Integer64(row.row_id as i64);
    }
    v.encode(out);
}

/// Build the `CellForMatcher` view over a single table row for restriction
/// evaluation. Cells already materialised in `row.cells` (for the current
/// `column_set`) are reused; if the row still carries its backend `source`
/// (the email/mailbox/attachment object) the cheap materialiser converts it
/// into cells once so a restriction referencing tags outside the chosen
/// column set still resolves. Returns the cells paired with their tags.
fn matcher_cells(
    row: &crate::mapi::session::TableRow,
    column_set: &[crate::mapi::data::PropertyTag],
    kind: FolderKind,
    mailbox_id: &str,
) -> Vec<CellForMatcher> {
    use crate::mapi::data::PropertyValue;
    let materialise =
        |src: &std::sync::Arc<dyn std::any::Any + Send + Sync>| -> Vec<PropertyValue> {
            if let Some(e) = src.downcast_ref::<crate::jmap::JmapEmail>() {
                store::email_to_cells(e, column_set, kind, mailbox_id)
            } else if let Some(m) = src.downcast_ref::<crate::jmap::JmapMailbox>() {
                store::mailbox_to_cells(m, column_set)
            } else if let Some(a) = src.downcast_ref::<crate::jmap::JmapAttachment>() {
                let num = u32::try_from(row.row_id).unwrap_or(0);
                store::attachment_to_cells(a, num, column_set)
            } else if let Some(c) = src.downcast_ref::<crate::calendar::CalendarItem>() {
                crate::mapi::converters::calendar_to_cells(c, column_set, mailbox_id)
            } else if let Some(v) = src.downcast_ref::<String>() {
                crate::mapi::converters::contact_to_cells(v, column_set, mailbox_id)
            } else {
                Vec::new()
            }
        };
    let cells = if !row.cells.is_empty() {
        row.cells.clone()
    } else if let Some(src) = row.source.as_ref() {
        materialise(src)
    } else {
        Vec::new()
    };
    column_set
        .iter()
        .zip(cells)
        .map(|(tag, value)| CellForMatcher { tag: *tag, value })
        .collect()
}

/// The indices of `rows` the active `restriction` admits. This is the ONE
/// filtered-view builder shared by `RopRestrict` (which derives the filtered
/// `total`) and `RopQueryRows` (which serves rows from it), so the count
/// `RopQueryPosition` reports always matches the rows `RopQueryRows` serves.
///
/// A restriction applied BEFORE `RopSetColumns` still resolves: the matcher
/// materialises cells over the UNION of the fixed column set and the tags the
/// restriction references (lifted from the row's cached backend source), so a
/// `PR_IMPORTANCE==5` filter over `PR_IMPORTANCE` works even when the client
/// has not yet fixed `PR_IMPORTANCE` into the column set. The `default()`
/// restriction (an empty `And`) references no tags and matches every row.
fn filtered_indices(
    rows: &[crate::mapi::session::TableRow],
    column_set: &[crate::mapi::data::PropertyTag],
    restriction: &SRestriction,
    kind: FolderKind,
    mailbox_id: &str,
) -> Vec<usize> {
    if matches!(restriction, SRestriction::And(v) if v.is_empty()) {
        return (0..rows.len()).collect();
    }
    // Union the fixed column set with the restriction-referenced tags so the
    // matcher sees a cell for every tag the restriction reads, regardless of
    // whether the client has fixed that tag into its column set yet.
    let mut cs: Vec<crate::mapi::data::PropertyTag> = column_set.to_vec();
    for t in restriction_referenced_tags(restriction) {
        if !cs.iter().any(|c| c.property_id == t.property_id) {
            cs.push(t);
        }
    }
    rows.iter()
        .enumerate()
        .filter(|(_, r)| restriction.matches(&matcher_cells(r, &cs, kind, mailbox_id)))
        .map(|(i, _)| i)
        .collect()
}

/// Apply a comparator over rows for the active `SortOrder` set. Stable sort
/// preserves the JMAP materialisation order for rows that compare equal.
/// Missing cells sort to the end (ascending) / the front (descending) to keep
/// Outlook's primary sort key (receivedAt, subject) actionable.
fn sort_rows(
    rows: &mut Vec<crate::mapi::session::TableRow>,
    sort_orders: &[SortOrder],
    column_set: &[crate::mapi::data::PropertyTag],
    kind: FolderKind,
    mailbox_id: &str,
) {
    if sort_orders.is_empty() {
        return;
    }
    // Pre-compute matcher cells per row once (cheap: already materialised in
    // most cases). We collect into a Vec to avoid re-materialising inside the
    // comparator (which borrows `rows` mutably otherwise).
    let per_row: Vec<Vec<CellForMatcher>> = rows
        .iter()
        .map(|r| matcher_cells(r, column_set, kind, mailbox_id))
        .collect();
    let mut idx: Vec<usize> = (0..rows.len()).collect();
    idx.sort_by(|&a, &b| {
        for so in sort_orders {
            let ca = per_row[a]
                .iter()
                .find(|c| c.tag.property_id == so.tag.property_id);
            let cb = per_row[b]
                .iter()
                .find(|c| c.tag.property_id == so.tag.property_id);
            let ord = match (ca, cb) {
                (Some(x), Some(y)) => scalar_ord_for_sort(&x.value, &y.value),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            };
            if !matches!(ord, std::cmp::Ordering::Equal) {
                // SortFlags bit 0x01 => descending (per MS-OXCDATA ┬¦2.12.1).
                return if so.sort_flags & 0x01 != 0 {
                    ord.reverse()
                } else {
                    ord
                };
            }
        }
        std::cmp::Ordering::Equal
    });
    let owned: Vec<crate::mapi::session::TableRow> = idx.iter().map(|&i| rows[i].clone()).collect();
    rows.clear();
    rows.extend(owned);
}

/// Order two scalar property values for sort. Falls back to Equal when the
/// values are not the same comparable family (the comparator then defers to
/// the next sort key, matching Outlook's multi-key behaviour). Delegates to
/// the single `restrict::scalar_ordering` comparator so the type-pair matrix
/// lives in ONE place (the restriction matcher) and cannot drift from the
/// sort path when a new `PropertyValue` variant lands.
fn scalar_ord_for_sort(
    a: &crate::mapi::data::PropertyValue,
    b: &crate::mapi::data::PropertyValue,
) -> std::cmp::Ordering {
    crate::mapi::restrict::scalar_ordering(a, b).unwrap_or(std::cmp::Ordering::Equal)
}

/// Serialise a contents-table (or hierarchy-table) row set as an MS-OXCFXICS
/// incremental-change ICS stream suitable for `RopFastTransferSourceGetBuffer`.
///
/// The stream shape is:
///   `IncrSyncChg` marker
///   per message: `IncrSyncMessage` marker + `propValue`* cells
///   `IncrSyncEnd` marker
///
/// Only the mail-folder case is exercised by New Outlook's primary sync; for
/// calendar/contacts tables the row `.source` carries a backend object the
/// materialiser converts via `email_to_cells` (mail) / `mailbox_to_cells`
/// (hierarchy). Cells use the FXICS `propValue` wire shape (tag-led), so the
/// client's FastTransfer consumer reconstructs every cached property without
/// a separate `Email/get` round-trip.
fn build_ics_stream(
    rows: &[crate::mapi::session::TableRow],
    column_set: &[crate::mapi::data::PropertyTag],
    kind: FolderKind,
    mailbox_id: &str,
) -> Vec<u8> {
    build_ics_stream_iter(rows.iter(), column_set, kind, mailbox_id)
}

/// `build_ics_stream` over a borrowed selection (used by CopyMessages when
/// the request carries an explicit `message_ids` list ŌĆö the selection is
/// a filtered view of `rows`, not a re-allocation of the rows themselves).
fn build_ics_stream_sel(
    rows: &[&crate::mapi::session::TableRow],
    column_set: &[crate::mapi::data::PropertyTag],
    kind: FolderKind,
    mailbox_id: &str,
) -> Vec<u8> {
    build_ics_stream_iter(rows.iter().copied(), column_set, kind, mailbox_id)
}

fn build_ics_stream_iter<'a, I>(
    rows: I,
    column_set: &[crate::mapi::data::PropertyTag],
    kind: FolderKind,
    mailbox_id: &str,
) -> Vec<u8>
where
    I: IntoIterator<Item = &'a crate::mapi::session::TableRow>,
{
    let mut b = IcsStreamBuilder::new();
    b.push_marker(Marker::IncrSyncChg);
    for r in rows {
        let cells = matcher_cells(r, column_set, kind, mailbox_id);
        b.push_marker(Marker::IncrSyncMessage);
        for c in &cells {
            let mut value_bytes = Vec::new();
            c.value.encode(&mut value_bytes);
            let tag_u32 =
                (c.tag.property_type.to_u16() as u32) | ((c.tag.property_id as u32) << 16);
            b.push_property(tag_u32, &value_bytes);
        }
        b.push_marker(Marker::EndMessage);
    }
    b.finish()
}

/// Classify the input handle for a FastTransfer source ROP and hand the live
/// table snapshot to `f`, which builds the ICS stream and installs the source
/// handle. Returns the typed ROP result:
/// - `Success` when `f` runs (it installs the source and returns Success).
/// - `NoSupport` when the input is `Handle::Folder` (the gateway has no
///   cached rows for a bare folder handle ŌĆö Outlook must open a
///   GetContentsTable/GetHierarchyTable first). The explicit `NoSupport`
///   distinguishes this from a missing handle so the failure mode is clear.
/// - `NotFound` when the input handle is absent or any other kind.
///
/// Rows are CLONED out of the table handle before `f` runs (the Arc-backed
/// `source` makes the clone cheap), so the immutable borrow of `s` ends and
/// `f` can borrow `&mut Session` (passed as its first arg, to install the
/// output handle) without an aliased borrow of the input handle's rows ŌĆö the
/// closure therefore captures NO `s` of its own.
fn fasttransfer_source_from_input<F>(
    s: &mut crate::mapi::session::Session,
    input_handle_index: u8,
    f: F,
) -> RopErrorCode
where
    F: FnOnce(
        &mut crate::mapi::session::Session,
        &[crate::mapi::session::TableRow],
        Vec<crate::mapi::data::PropertyTag>,
        FolderKind,
        &str,
    ) -> RopErrorCode,
{
    enum Snap {
        Rows(
            Vec<crate::mapi::session::TableRow>,
            Vec<crate::mapi::data::PropertyTag>,
            FolderKind,
            String,
        ),
        Folder,
        None,
    }
    let snap = match s.handle(input_handle_index) {
        Some(crate::mapi::session::Handle::Table {
            rows,
            column_set,
            kind,
            parent_backend_id,
            ..
        }) => Snap::Rows(
            rows.clone(),
            column_set.clone(),
            *kind,
            parent_backend_id.clone(),
        ),
        Some(crate::mapi::session::Handle::Folder { .. }) => Snap::Folder,
        _ => Snap::None,
    };
    match snap {
        Snap::Rows(rows, cs, kind, mb) => f(s, &rows, cs, kind, &mb),
        Snap::Folder => RopErrorCode::NoSupport,
        Snap::None => RopErrorCode::NotFound,
    }
}

/// Apply a completed FastTransfer upload stream to the JMAP backend.
///
/// Phase-2 write-back bridge for FXICS (MS-OXCFXICS) upload, closing audit
/// gap #2: the uploader ROP chain accumulates the full ICS byte stream on the
/// destination handle; this helper tokenises it (FXICS) and converts the
/// resulting `FxEvent` sequence into JMAP `Email/set` / `Email/destroy`
/// operations against Stalwart, reusing the existing `JmapClient` primitives
/// (`create_email`, `update_email`, `destroy_emails`, `move_emails`,
/// `list_email_ids_in_mailbox`) — no new dependencies.
///
/// Translated events:
///   * `IncrSyncRead`  ─-> `Email/set { keywords/$seen }` (read-state batch).
///   * `IncrSyncDel`   ─-> `Email/destroy` (deletion batch). The deleted-message
///     ids are carried as `PR_MID` (PtypInteger64) `propValue` cells inside the
///     `IncrSyncDel` span. The MAPI->JMAP id mapping is one-way (a stable
///     FNV-1a hash), so the reverse lookup is done ONCE per apply by enumerating
///     the parent folder (`list_email_ids_in_mailbox`) and matching by
///     `message_id_from_jmap` — exactly the pattern `RopDeleteMessages` uses.
///   * `IncrSyncMessage` (full message change inside an `IncrSyncChg`) ─-> if
///     the message id resolves to an existing JMAP email, a property `Email/set`
///     `update` patch for the cleanly-mappable MAPI property subset (read flag
///     via `PR_MESSAGE_FLAGS`/`PR_READ`, follow-up flag via `PR_FLAG_STATUS`,
///     subject via `PR_SUBJECT`, mailbox move via a changed `PR_FOLDER_ID`).
///     A message id that does NOT resolve (a brand-new message the client
///     composed over the bulk upload) is best-effort: a full JMAP Email object
///     cannot always be synthesised from the summary-column cells the gateway
///     served on download, so the create is attempted only when the bag carries
///     the recognised editable props and logged otherwise, never aborting the
///     rest of the upload (audit gap #3 — MIME/body Blob-upload write-back —
///     is the natural owner of the full create path).
///
/// The apply is intentionally best-effort and tolerant per event: an
/// unrecognised or untranslatable event is `tracing::warn`-logged (folder id
/// redacted) and skipped rather than aborting the client stream, because
/// Outlook issues a wide and version-dependent set of ICS elements and failing
/// the whole upload would silently swallow the items the client already
/// prepared to send. A *malformed* FXICS byte stream (decode failure mid-walk)
/// fails closed (`Err(DecodeError)`) so the dispatcher reports a typed
/// `DiskError` rather than masking a corrupt stream as a clean apply —
/// matching the contract the previous best-effort tokenizer already guarded.
///
/// When the JMAP client / credentials are absent (unit-test path or no JMAP
/// backend configured) the apply tokenises + logs but performs no backend
/// writes, so the ROP stream still completes cleanly — the established
/// "no-backend -> tokenize-only" contract.
async fn apply_fasttransfer_upload(
    jc: Option<&crate::jmap::JmapClient>,
    password: Option<&secrecy::SecretString>,
    username: &str,
    buffer: &[u8],
    source_fmt: u8,
    parent_backend_id: &str,
) -> Result<(), DecodeError> {
    use crate::mapi::fxics::{FxEvent, Marker};
    use std::collections::HashMap;

    // ── Resolve the JMAP account id once (the JMAP session cache inside
    //    JmapClient amortises the auth handshake per-username for 5 min).
    //    When JMAP is absent we still walk the tokenizer for the fail-closed
    //    contract; no writes are issued.
    let account_id = if let (Some(jc), Some(pw)) = (jc, password) {
        match jc.get_account_id(username, pw).await {
            Ok(a) if !a.is_empty() => a,
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let wired = jc.is_some() && password.is_some() && !account_id.is_empty();
    if !wired && !buffer.is_empty() {
        tracing::debug!("fasttransfer upload tokenize-only (no JMAP backend / creds / account id)");
    }

    // ── Build the MAPI-mid -> JMAP-id reverse map ONCE for the parent folder.
    //    Every IncrSyncDel / IncrSyncRead / IncrSyncMessage event resolves
    //    against this map so the upload triggers at most ONE folder
    //    enumeration plus one Email/set per *batched* event, never a
    //    per-message query.
    let mut mid_to_jmap: HashMap<u64, (String, Vec<String>)> = HashMap::new();
    if let (Some(jc), Some(pw), true) = (jc, password, wired)
        && let Ok(list) = jc
            .list_email_ids_in_mailbox(&account_id, parent_backend_id, username, pw)
            .await
    {
        for (jid, mids) in list {
            mid_to_jmap.insert(store::message_id_from_jmap(&jid), (jid, mids));
        }
    }

    // ── Build the FOLDER-mid -> JMAP-mailbox-id reverse map for cross-folder
    //    moves. An `IncrSyncMessage` bag carries the destination as a
    //    `PR_FOLDER_ID` PtypInteger64 whose value is
    //    `store::folder_id_from_backend(jmap_mailbox_id)` (the hierarchy
    //    table assigns `backend_id = mbx.id`, line ~848). The message-id
    //    map above keys on `message_id_from_jmap(email_jid)` — a DIFFERENT
    //    u64 hash — so resolving the move destination against `mid_to_jmap`
    //    can never match. `folder_mid_tomailbox` is built once from
    //    `query_mailboxes` so the move fires with the real JMAP mailbox id
    //    and the right `mailboxIds/<id>` PatchObject keys (RFC 8620 — the
    //    leading slash is implicit, never written).
    let mut folder_mid_to_mailbox: HashMap<u64, String> = HashMap::new();
    if let (Some(jc), Some(pw), true) = (jc, password, wired)
        && let Ok(ml) = jc.query_mailboxes(username, pw).await
    {
        for mbx in ml.mailboxes {
            if let Some(id) = mbx.id.as_deref()
                && !id.is_empty()
            {
                folder_mid_to_mailbox.insert(store::folder_id_from_backend(id), id.to_string());
            }
        }
    }

    // ── Walk the event stream. The FXICS event sequence for an upload is a
    //    series of top-level Marker::IncrSyncChg / IncrSyncDel / IncrSyncRead
    //    spans, each terminated by an IncrSyncEnd marker (the download producer
    //    `build_ics_stream_iter` emits the same shape). We track the active
    //    span so property cells route into the right accumulator; the closing
    //    IncrSyncEnd flushes the pending span to JMAP.
    let mut tok = Tokenizer::new(buffer);
    let mut stats = FxApplyStats::default();
    let mut span: FxSpan = FxSpan::Idle;
    let mut read_pairs: Vec<(u64, bool)> = Vec::new();
    let mut chg_bags: Vec<FxMessageBag> = Vec::new();
    let mut del_mids: Vec<u64> = Vec::new();
    // Pending (mid, read) within an IncrSyncRead span; flushed into
    // `read_pairs` when the pair is complete.
    let mut pending_mid: Option<u64> = None;
    // The JMAP dispatch context — built once and shared by every flush (the
    // reverse id map + account id + wired flag were resolved above).
    let ctx = FxApplyCtx {
        jc,
        password,
        username,
        account_id: &account_id,
        parent_backend_id,
        mid_to_jmap: &mid_to_jmap,
        folder_mid_to_mailbox: &folder_mid_to_mailbox,
        wired,
    };

    while let Some(ev) = tok.next_event()? {
        stats.events = stats.events.saturating_add(1);
        match ev {
            FxEvent::Marker(m) => match m {
                Marker::IncrSyncChg | Marker::IncrSyncChgPartial => span = FxSpan::Chg,
                Marker::IncrSyncDel => span = FxSpan::Del,
                Marker::IncrSyncRead => {
                    span = FxSpan::Read;
                    pending_mid = None;
                }
                Marker::IncrSyncEnd => {
                    flush_span(
                        &ctx,
                        &span,
                        &mut read_pairs,
                        &mut chg_bags,
                        &mut del_mids,
                        &mut stats,
                    )
                    .await;
                    span = FxSpan::Idle;
                    pending_mid = None;
                }
                Marker::IncrSyncMessage | Marker::StartMessage | Marker::StartFaiMsg => {
                    if matches!(span, FxSpan::Chg) {
                        chg_bags.push(FxMessageBag::default());
                    }
                }
                // EndMessage closes the innermost change-message bag; the
                // bag is left in place and applied at the IncrSyncEnd flush
                // (a single batched Email/set call covers every message in
                // the span). Hierarchy-only markers (StartTopFld/StartSubFld/
                // EndFolder) bound a folder-sync upload with no Email/JMAP
                // analogue (Stalwart mailboxes are server-managed) and are
                // tolerated as context only.
                Marker::EndMessage
                | Marker::StartTopFld
                | Marker::StartSubFld
                | Marker::EndFolder
                | Marker::StartEmbed
                | Marker::EndEmbed
                | Marker::StartRecip
                | Marker::EndToRecip
                | Marker::NewAttach
                | Marker::EndAttach
                | Marker::IncrSyncStateBegin
                | Marker::IncrSyncStateEnd
                | Marker::IncrSyncProgressMode
                | Marker::IncrSyncProgressPerMsg
                | Marker::IncrSyncGroupInfo
                | Marker::FxErrorInfo
                | Marker::Unknown(_) => {}
            },
            FxEvent::Property { tag, bytes } => match span {
                FxSpan::Idle => {}
                FxSpan::Chg => {
                    if let Some(bag) = chg_bags.last_mut() {
                        bag.push(tag, bytes);
                    }
                }
                FxSpan::Del => {
                    if let Some(mid) = fx_decode_mid_cell(tag, &bytes) {
                        del_mids.push(mid);
                    }
                }
                FxSpan::Read => {
                    let pid = ((tag >> 16) & 0xFFFF) as u16;
                    if pid == store::PR_MID {
                        if let Some(mid) = fx_decode_raw_mid(&bytes) {
                            pending_mid = Some(mid);
                        }
                    } else {
                        // Decode the read-state cell BEFORE taking the pending
                        // mid: an FXICS read span can interleave unrelated
                        // property cells (e.g. PR_CHANGE_KEY /
                        // PR_LAST_MODIFICATION_TIME) between PR_MID and the
                        // read-flag cell, and `pending_mid.take()` evaluated
                        // eagerly inside a tuple would silently discard the
                        // mid when the interleaved cell does not decode to a
                        // bool — dropping the read-state update with no
                        // warning. Only consume the mid once a `read` value is
                        // actually available.
                        let read = if pid == store::PR_MESSAGE_FLAGS {
                            fx_decode_i32(&bytes).map(|f| f & 0x40 != 0)
                        } else {
                            fx_decode_bool(&bytes)
                        };
                        if let Some(r) = read
                            && let Some(mid) = pending_mid.take()
                        {
                            read_pairs.push((mid, r));
                        }
                    }
                }
            },
        }
    }
    // A stream with no trailing IncrSyncEnd still flushes any unflushed span
    // (a tolerant Outlook may omit the final terminator on some sub-streams);
    // the fail-closed `assert_complete` below rejects a structurally broken
    // stream whose start markers were never closed.
    if !matches!(span, FxSpan::Idle) {
        flush_span(
            &ctx,
            &span,
            &mut read_pairs,
            &mut chg_bags,
            &mut del_mids,
            &mut stats,
        )
        .await;
    }
    tok.assert_complete()?;

    tracing::debug!(
        events = stats.events,
        reads_applied = stats.reads_applied,
        dels_applied = stats.dels_applied,
        chgs_applied = stats.chgs_applied,
        skipped = stats.skipped,
        source_fmt,
        "fasttransfer upload apply (FXICS -> JMAP write-back)"
    );
    let _ = parent_backend_id;
    Ok(())
}

/// State for the FXICS upload apply walk. Counters feed the `tracing::debug`
/// summary; none are surfaced to the client.
#[derive(Default)]
struct FxApplyStats {
    events: u32,
    reads_applied: u32,
    dels_applied: u32,
    chgs_applied: u32,
    skipped: u32,
}

/// Active top-level FXICS span being walked.
#[derive(Clone, Copy, PartialEq)]
enum FxSpan {
    Idle,
    Chg,
    Del,
    Read,
}

/// Property bag for one `IncrSyncMessage` change inside an `IncrSyncChg`
/// span. Cells are kept as raw `(tag, bytes)` pairs — the FXICS `propValue`
/// wire form differs from the MS-OXCDATA ROP-buffer `PropertyValue` form
/// (no 2-byte count + NUL for strings), so the cells are interpreted by
/// dedicated FXICS-aware decoders rather than `PropertyValue::decode`.
#[derive(Default)]
struct FxMessageBag {
    cells: Vec<(u32, Vec<u8>)>,
}

impl FxMessageBag {
    fn push(&mut self, tag: u32, bytes: Vec<u8>) {
        self.cells.push((tag, bytes));
    }

    /// The candidate MAPI message id for this bag, decoded from the
    /// `PR_MID` cell if present (the authoritative id the gateway emits on
    /// download). `None` indicates a cell-free message or one carried only
    /// by a `PR_SOURCE_KEY` binary — the latter is not currently emitted by
    /// the gateway's download producer, so such a bag is treated as a
    /// best-effort create (and usually skipped).
    fn mid(&self) -> Option<u64> {
        self.cells
            .iter()
            .find(|(t, _)| fx_is_mid_cell(*t))
            .and_then(|(_, b)| fx_decode_raw_mid(b))
    }

    /// Build a JMAP `Email/set` `update` patch value for this bag (the inner
    /// `{ <field>: <value> }` object), or `None` if no translatable property
    /// is present. Reuses the per-property translation logic shared with the
    /// `RopSetMessageReadFlag` / `RopSetProperties` paths.
    fn to_jmap_patch(&self) -> Option<serde_json::Value> {
        let mut patch = serde_json::Map::new();
        for (tag, bytes) in &self.cells {
            let property_type = crate::mapi::data::PropertyType::from_u16((tag & 0xFFFF) as u16);
            let property_id = ((tag >> 16) & 0xFFFF) as u16;
            match property_id {
                store::PR_SUBJECT => {
                    if let Some(s) = fx_decode_string(property_type, bytes) {
                        patch.insert("subject".to_string(), serde_json::Value::String(s));
                    }
                }
                store::PR_MESSAGE_FLAGS => {
                    // MSGFLAG_READ (0x40) bit of the Integer32 flags word.
                    if let Some(flags) = fx_decode_i32(bytes) {
                        let read = flags & 0x40 != 0;
                        patch.insert(
                            "keywords/$seen".to_string(),
                            if read {
                                serde_json::json!(true)
                            } else {
                                serde_json::Value::Null
                            },
                        );
                    }
                }
                store::PR_READ => {
                    if let Some(b) = fx_decode_bool(bytes) {
                        patch.insert(
                            "keywords/$seen".to_string(),
                            if b {
                                serde_json::json!(true)
                            } else {
                                serde_json::Value::Null
                            },
                        );
                    }
                }
                store::PR_FLAG_STATUS => {
                    if let Some(v) = fx_decode_i32(bytes) {
                        // MS-OXOFLAG 2.2.1.1: 0x02 followupFlagged sets the
                        // flag; any other value clears it.
                        let set = v == 0x02;
                        patch.insert(
                            "keywords/$flagged".to_string(),
                            if set {
                                serde_json::json!(true)
                            } else {
                                serde_json::Value::Null
                            },
                        );
                    }
                }
                store::PR_IMPORTANCE => {
                    if let Some(v) = fx_decode_i32(bytes) {
                        let important = v == 2;
                        patch.insert(
                            "keywords/$important".to_string(),
                            if important {
                                serde_json::json!(true)
                            } else {
                                serde_json::Value::Null
                            },
                        );
                    }
                }
                _ => {}
            }
        }
        if patch.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(patch))
        }
    }

    /// Resolve the destination folder id for a move: if the bag carries a
    /// `PR_FOLDER_ID` that does NOT match the parent folder of this upload,
    /// the message is being moved into that folder. Returns the MAPI folder
    /// id of the destination (the gateway's per-folder upload flush would
    /// resolve it to a JMAP mailbox id via a second reverse lookup, but a
    /// cross-folder mailboxIds patch uses the JMAP id directly once known).
    fn move_dest_mid(&self, parent_mid: u64) -> Option<u64> {
        self.cells
            .iter()
            .find(|(t, _)| {
                ((t >> 16) & 0xFFFF) as u16 == store::PR_FOLDER_ID
                    || ((t >> 16) & 0xFFFF) as u16 == store::PR_PARENT_FOLDER_ID
            })
            .and_then(|(_, b)| fx_decode_raw_mid(b))
            .filter(|&m| m != parent_mid && m != 0)
    }
}

/// Shared JMAP dispatch context for `flush_span`: holds the JMAP client,
/// credentials, account id, parent folder backend id, the pre-built reverse
/// mid id map, and the wired flag. Grouped into a struct so `flush_span`
/// stays under clippy's argument-count threshold (the established handler
/// `execute_one_rop` carries 13 args + `#[allow]`; the apply path instead
/// bundles them here so no suppression is needed).
struct FxApplyCtx<'a> {
    jc: Option<&'a crate::jmap::JmapClient>,
    password: Option<&'a secrecy::SecretString>,
    username: &'a str,
    account_id: &'a str,
    parent_backend_id: &'a str,
    mid_to_jmap: &'a std::collections::HashMap<u64, (String, Vec<String>)>,
    /// FOLDER-mid (`folder_id_from_backend(jmap_mailbox_id)`) -> JMAP
    /// mailbox id. Resolves the `PR_FOLDER_ID` destination of a cross-folder
    /// move; keys on a Different hash than `mid_to_jmap` (message ids).
    folder_mid_to_mailbox: &'a std::collections::HashMap<u64, String>,
    wired: bool,
}

impl FxApplyCtx<'_> {
    /// The `(jc, password)` pair when both are present (every JMAP call needs
    /// both); `None` otherwise so the caller short-circuits without a borrow.
    fn backend(&self) -> Option<(&crate::jmap::JmapClient, &secrecy::SecretString)> {
        match (self.jc, self.password) {
            (Some(jc), Some(pw)) => Some((jc, pw)),
            _ => None,
        }
    }
}

/// Build the batched `Email/set` `update` object for an `IncrSyncRead` span.
/// Pure (no I/O) so the exact payload — RFC 8621 §4.5 `keywords/$seen` patch
/// keyed by JMAP id; the leading slash of the PatchObject key is implicit
/// (RFC 8620 §5.3), so the wire key is `keywords/$seen`, NOT
/// `/keywords/$seen` — can be asserted without a live server. A mid the
/// reverse map cannot resolve is skipped (returned in `skipped`).
///
/// Returns `(update_payload, applied_count, skipped_count)`.
fn fx_build_read_update(
    mid_to_jmap: &std::collections::HashMap<u64, (String, Vec<String>)>,
    read_pairs: &[(u64, bool)],
) -> (serde_json::Value, u32, u32) {
    let mut update = serde_json::Map::new();
    let mut applied = 0u32;
    let mut skipped = 0u32;
    for (mid, read) in read_pairs {
        let Some((jid, _)) = mid_to_jmap.get(mid) else {
            skipped = skipped.saturating_add(1);
            continue;
        };
        let mut patch = serde_json::Map::new();
        patch.insert(
            "keywords/$seen".to_string(),
            if *read {
                serde_json::json!(true)
            } else {
                serde_json::Value::Null
            },
        );
        update.insert(jid.clone(), serde_json::Value::Object(patch));
        applied = applied.saturating_add(1);
    }
    (serde_json::Value::Object(update), applied, skipped)
}

/// Build the `Email/set` `update` patch object for one cross-folder move.
/// Pure. The returned object's keys are `mailboxIds/<id>` with NO leading
/// slash (RFC 8620 PatchObject keys have an implicit leading slash; the
/// canonical `build_move_update_patch` in `jmap.rs` uses this exact form):
/// `<target>: true` plus `<current>: null` for each mailbox id the email
/// currently lives in. The caller resolves the destination folder mid to a
/// JMAP mailbox id via `folder_mid_to_mailbox`; when it does not resolve, no
/// move patch is emitted.
fn fx_build_move_update(
    current_mids: &[String],
    dest_mailbox_id: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let mut patch = serde_json::Map::new();
    // RFC 8621 §4.5 move semantics: add target, null every current id.
    // Keys carry NO leading slash — RFC 8620 PatchObject keys are implicit.
    for old in current_mids {
        if old != dest_mailbox_id {
            patch.insert(format!("mailboxIds/{old}"), serde_json::Value::Null);
        }
    }
    patch.insert(
        format!("mailboxIds/{dest_mailbox_id}"),
        serde_json::json!(true),
    );
    patch
}

/// Dispatch the pending top-level FXICS span to JMAP. Read-state changes and
/// deletes are batched into single `Email/set` / `Email/destroy` calls (one
/// round-trip per span); change messages are applied incrementally with a
/// patch per resolved mid. Every failure is `tracing::warn`-logged and
/// counted as skipped — the apply never aborts the rest of an upload over
/// one bad event.
async fn flush_span(
    ctx: &FxApplyCtx<'_>,
    span: &FxSpan,
    read_pairs: &mut Vec<(u64, bool)>,
    chg_bags: &mut Vec<FxMessageBag>,
    del_mids: &mut Vec<u64>,
    stats: &mut FxApplyStats,
) {
    match span {
        FxSpan::Idle => {}
        FxSpan::Read => {
            if !ctx.wired {
                stats.skipped = stats
                    .skipped
                    .saturating_add(u32::try_from(read_pairs.len()).unwrap_or(u32::MAX));
                read_pairs.clear();
                return;
            }
            let Some((jc, pw)) = ctx.backend() else {
                return;
            };
            // Build the batched `Email/set` `update` object for every
            // read-state change keyed by JMAP id (RFC 8621 §4.5). A mid that
            // does not resolve (the folder enumeration at apply start missed
            // it — e.g. it was moved/deleted since) is skipped, not failed.
            let (keyed, applied, skipped) = fx_build_read_update(ctx.mid_to_jmap, read_pairs);
            stats.skipped = stats.skipped.saturating_add(skipped);
            read_pairs.clear();
            if let Some(obj) = keyed.as_object()
                && !obj.is_empty()
            {
                // Use the inspected `update_email_checked` so per-id
                // `notUpdated` rejections surface in stats + warnings instead
                // of being masked as success (the unchecked `update_email`
                // returns Ok(()) on a method-level-only success, the path
                // RopSetProperties migrated away from).
                match jc
                    .update_email_checked(ctx.account_id, &keyed, ctx.username, pw)
                    .await
                {
                    Ok(outcome) => {
                        let updated = u32::try_from(outcome.updated.len()).unwrap_or(u32::MAX);
                        stats.reads_applied = stats.reads_applied.saturating_add(updated);
                        // Every requested id the server did NOT accept is a
                        // masked rejection: count it as skipped and warn per
                        // pair so the silent loss is visible. A method-level
                        // error rejects the whole span.
                        let rejected = applied.saturating_sub(updated);
                        stats.skipped = stats.skipped.saturating_add(rejected);
                        for (id, desc) in &outcome.not_updated {
                            tracing::warn!(
                                email_id = %id,
                                reason = %desc,
                                "FXICS IncrSyncRead Email/set notUpdated"
                            );
                        }
                        if let Some(err) = &outcome.method_error {
                            tracing::warn!(error = %err, "FXICS IncrSyncRead Email/set method error");
                            stats.skipped = stats.skipped.saturating_add(updated);
                            stats.reads_applied = 0;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "FXICS IncrSyncRead Email/set failed");
                        stats.skipped = stats.skipped.saturating_add(applied);
                    }
                }
            }
        }
        FxSpan::Del => {
            if !ctx.wired || del_mids.is_empty() {
                stats.skipped = stats
                    .skipped
                    .saturating_add(u32::try_from(del_mids.len()).unwrap_or(u32::MAX));
                del_mids.clear();
                return;
            }
            let Some((jc, pw)) = ctx.backend() else {
                return;
            };
            let mut to_destroy: Vec<String> = Vec::new();
            let mut unresolved = 0u32;
            for mid in del_mids.drain(..) {
                match ctx.mid_to_jmap.get(&mid) {
                    Some((jid, _)) => to_destroy.push(jid.clone()),
                    None => unresolved = unresolved.saturating_add(1),
                }
            }
            stats.skipped = stats.skipped.saturating_add(unresolved);
            if to_destroy.is_empty() {
                return;
            }
            match jc
                .destroy_emails(ctx.account_id, &to_destroy, ctx.username, pw)
                .await
            {
                Ok(destroyed) => {
                    stats.dels_applied = stats
                        .dels_applied
                        .saturating_add(u32::try_from(destroyed).unwrap_or(u32::MAX));
                    let missing = to_destroy.len().saturating_sub(destroyed);
                    stats.skipped = stats
                        .skipped
                        .saturating_add(u32::try_from(missing).unwrap_or(u32::MAX));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "FXICS IncrSyncDel Email/destroy failed");
                    stats.skipped = stats
                        .skipped
                        .saturating_add(u32::try_from(to_destroy.len()).unwrap_or(u32::MAX));
                }
            }
        }
        FxSpan::Chg => {
            if !ctx.wired {
                stats.skipped = stats
                    .skipped
                    .saturating_add(u32::try_from(chg_bags.len()).unwrap_or(u32::MAX));
                chg_bags.clear();
                return;
            }
            let Some((jc, pw)) = ctx.backend() else {
                return;
            };
            for bag in chg_bags.drain(..) {
                let Some(mid) = bag.mid() else {
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                };
                let Some((jid, current_mids)) = ctx.mid_to_jmap.get(&mid).cloned() else {
                    stats.skipped = stats.skipped.saturating_add(1);
                    continue;
                };
                let mut applied = false;
                // Property patch (read flag / follow-up / importance / subject).
                if let Some(patch_value) = bag.to_jmap_patch() {
                    let keyed = serde_json::json!({ jid.clone(): patch_value });
                    match jc
                        .update_email_checked(ctx.account_id, &keyed, ctx.username, pw)
                        .await
                    {
                        Ok(outcome) => {
                            if outcome.updated.iter().any(|u| u == &jid) {
                                applied = true;
                            } else {
                                for (id, desc) in &outcome.not_updated {
                                    tracing::warn!(
                                        email_id = %id,
                                        reason = %desc,
                                        "FXICS IncrSyncMessage Email/set patch notUpdated"
                                    );
                                }
                                if let Some(err) = &outcome.method_error {
                                    tracing::warn!(
                                        error = %err,
                                        "FXICS IncrSyncMessage Email/set patch method error"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "FXICS IncrSyncMessage Email/set patch failed"
                            );
                        }
                    }
                }
                // Cross-folder move: a PR_FOLDER_ID differing from the parent.
                // The destination is a FOLDER mid (folder_id_from_backend of
                // the target mailbox id), so it must be resolved against the
                // folder_mid->mailbox map — NOT the message-id map (the two
                // hash families are distinct; looking the folder mid up in
                // mid_to_jmap can never match).
                let parent_mid = store::folder_id_from_backend(ctx.parent_backend_id);
                if let Some(dest_mid) = bag.move_dest_mid(parent_mid)
                    && let Some(dest_mailbox_id) = ctx.folder_mid_to_mailbox.get(&dest_mid).cloned()
                {
                    // mailboxIds patch: add target, null current. RFC 8621
                    // §4.5 Email/set PatchObject keys have an IMPLICIT leading
                    // slash (RFC 8620 §5.3), so the wire key is
                    // `mailboxIds/<id>`, NOT `/mailboxIds/<id>` (every other
                    // mailboxIds patch site in the repo uses this form).
                    let mids_patch = fx_build_move_update(&current_mids, &dest_mailbox_id);
                    let keyed =
                        serde_json::json!({ jid.clone(): serde_json::Value::Object(mids_patch) });
                    match jc
                        .update_email_checked(ctx.account_id, &keyed, ctx.username, pw)
                        .await
                    {
                        Ok(outcome) => {
                            if outcome.updated.iter().any(|u| u == &jid) {
                                applied = true;
                            } else {
                                for (id, desc) in &outcome.not_updated {
                                    tracing::warn!(
                                        email_id = %id,
                                        reason = %desc,
                                        "FXICS IncrSyncMessage mailbox move notUpdated"
                                    );
                                }
                                if let Some(err) = &outcome.method_error {
                                    tracing::warn!(
                                        error = %err,
                                        "FXICS IncrSyncMessage mailbox move method error"
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "FXICS IncrSyncMessage mailbox move failed"
                            );
                        }
                    }
                }
                if applied {
                    stats.chgs_applied = stats.chgs_applied.saturating_add(1);
                } else {
                    stats.skipped = stats.skipped.saturating_add(1);
                }
            }
        }
    }
}

/// Decode an FXICS `propValue` cell whose property id is a candidate
/// `PR_MID` (PtypInteger64, 8 bytes) into a MAPI message id. Returns `None`
/// when the tag is not an Integer64 message-id cell or the payload is
/// truncated.
fn fx_decode_mid_cell(tag: u32, bytes: &[u8]) -> Option<u64> {
    if !fx_is_mid_cell(tag) {
        return None;
    }
    fx_decode_raw_mid(bytes)
}

/// Whether a FXICS property tag names a message-id (PtypInteger64 with id
/// `PR_MID`). `PR_SOURCE_KEY` (a binary) is not emitted by the gateway's
/// download producer and is intentionally not treated as an id here.
fn fx_is_mid_cell(tag: u32) -> bool {
    let property_type = crate::mapi::data::PropertyType::from_u16((tag & 0xFFFF) as u16);
    let property_id = ((tag >> 16) & 0xFFFF) as u16;
    property_id == store::PR_MID && property_type == crate::mapi::data::PropertyType::PTYP_INTEGER64
}

/// Decode a raw 8-byte LE Integer64 payload (the FXICS `propValue` raw form
/// for `PtypInteger64`) into a `u64` message/folder id.
fn fx_decode_raw_mid(bytes: &[u8]) -> Option<u64> {
    let raw = bytes.get(..8)?;
    Some(u64::from_le_bytes([
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
    ]))
}

/// Decode an FXICS `propValue` Integer32 cell (4 bytes LE) into `i32`.
fn fx_decode_i32(bytes: &[u8]) -> Option<i32> {
    let raw = bytes.get(..4)?;
    Some(i32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

/// Decode an FXICS `propValue` boolean cell (2 bytes; nonzero = true per the
/// FXICS `PtypBoolean` 2-byte form, MS-OXCFXICS §2.2.4.1.3) into a `bool`.
fn fx_decode_bool(bytes: &[u8]) -> Option<bool> {
    let raw = bytes.get(..2)?;
    Some(raw[0] != 0 || raw[1] != 0)
}

/// Decode an FXICS `propValue` string cell (`PtypString`/`PtypString8`) into
/// a `String`. The FXICS `propValue` raw form for strings is the raw UTF-16LE
/// / codepage byte payload (NO NUL terminator and NO inner count, per the
/// Tokenizer's `read_property_payload`).
fn fx_decode_string(
    property_type: crate::mapi::data::PropertyType,
    bytes: &[u8],
) -> Option<String> {
    use crate::mapi::data::PropertyType as T;
    match property_type {
        T::PTYP_STRING => {
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Some(String::from_utf16_lossy(&units))
        }
        T::PTYP_STRING8 => Some(String::from_utf8_lossy(bytes).into_owned()),
        _ => None,
    }
}

/// `Disconnect` RPC: drop the session if present, return 0.
async fn handle_disconnect(req: MapiRequest, state: &MapiState) -> MapiResponse {
    // Per MS-OXCMAPIHTTP ┬¦3.2.5.5 the Session Context is identified by the
    // `Cookie: MapiContext=<opaque>` header echoed from Connect; fall back to
    // the X-ClientInfo extension UUID for the in-process unit path.
    let id = crate::mapi::transport::cookie_value(&req.cookies, "MapiContext")
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .or_else(|| {
            req.client_info
                .as_deref()
                .and_then(|info| uuid::Uuid::parse_str(info.split(':').next().unwrap_or(info)).ok())
        });
    if let Some(id) = id {
        state.sessions.remove(&id);
    }
    MapiResponse::success(req.request_id, "Disconnect", None, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    // The full transportŌåÆlogon plumbing is exercised in the integration
    // test target against the real AuthVerifier; the unit tests below cover
    // the dispatcher's co-validation failures.
    #[tokio::test]
    async fn endpoint_disabled_returns_16() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = false;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Connect),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(),
            body: Vec::new(),
        };
        // The handler short-circuits before parsing because mapi_enabled is
        // false; the transport layer normally rejects at parse_request first.
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::EndpointDisabled);
    }

    #[tokio::test]
    async fn ping_returns_success() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Ping),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(),
            body: Vec::new(),
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
    }

    #[tokio::test]
    async fn connect_with_empty_body_is_invalid() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Connect),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(),
            body: Vec::new(),
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::InvalidRequestBody);
    }

    #[tokio::test]
    async fn execute_without_client_info_rejected() {
        // The transport requires X-ClientInfo (carrying a session id) once
        // Execute-ROP dispatch is live; absent client_info maps to a
        // transport-level InvalidRequestBody.
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(),
            body: vec![0xFF, 7],
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::InvalidRequestBody);
    }

    #[tokio::test]
    async fn execute_unknown_session_emits_rop_not_found() {
        // An Execute RPC whose X-ClientInfo references a session that is no
        // longer live yields a transport success wrapping a single ROP-level
        // NotFound so the client re-Connects. This models the MAPI/HTTP wire
        // correctly: transport success, ROP failure.
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let stray = uuid::Uuid::new_v4();
        let body: Vec<u8> = vec![0xFF, 7]; // RopId(0xFF) + handle index
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", stray.as_hyphenated())),
            username: None,
            password: None,
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0xFF); // echoed RopId
        assert_eq!(payload[1], 7); // echoed handle index
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NotFound);
    }

    #[tokio::test]
    async fn execute_rop_release_round_trips() {
        // A live session + a RopRelease chain. We expect ONE transport success
        // whose body begins with the RopRelease response (RopId 0x01 +
        // InputHandleIndex + Success return value).
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                3,
                crate::mapi::session::Handle::Folder {
                    backend_id: "I".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                },
            );
        });
        // RopRelease wire: RopId(0x01) ┬Ę LogonId(0) ┬Ę InputHandleIndex(3)
        let body: Vec<u8> = vec![0x01, 0, 3];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: None,
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x01); // RopId echoed
        assert_eq!(payload[1], 3); // InputHandleIndex echoed
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        // The handle was freed by the dispatcher.
        let snap = state.sessions.get(&sid).expect("session");
        assert!(!snap.handles.contains_key(&3));
    }

    /// End-to-end MAPI notification flow (audit §2e):
    /// 1. `NotificationWait` with a pending event returns `EventPending=1`.
    /// 2. A subsequent (empty-body) `Execute` drains the queued event into a
    ///    `RopNotify` (RopId 0x2A) carrying the 4-byte NotificationHandle.
    #[tokio::test]
    async fn notification_wait_reports_pending_and_execute_drains_rop_notify() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let mgr = std::sync::Arc::new(crate::notifications::SubscriptionManager::new());
        let state = MapiState::with_subscription_manager(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
            mgr.clone(),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });

        // Install a notification sink directly via the registry (mirrors what
        // `RopRegisterNotification` does): whole-store, NewMail only, handle
        // index 4, logon id 2.
        state.sessions.notifications().register(
            sid,
            4,
            crate::mapi::session::MapiNotificationSink::new(
                "u@example.com".into(),
                crate::mapi::session::NT_NEW_MAIL,
                crate::mapi::session::NotificationScope::WholeStore,
                2,
                mgr.subscribe_raw(),
            ),
        );

        // Publish a matching NewMail event BEFORE the wait so the
        // `notification_wait_poll`'s initial pump returns EventPending=1
        // immediately (no long-poll wait in the test).
        mgr.publish(crate::notifications::NotificationEvent::NewMail {
            owner: "u@example.com".into(),
            folder_id: "inbox".into(),
            item_id: "M-42".into(),
            change_key: String::new(),
        });

        // NotificationWait request: Flags(4)=0 · AuxBufSize(4)=0, cookie-bound.
        let wait_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::NotificationWait),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: {
                let mut b = Vec::new();
                b.extend_from_slice(&0u32.to_le_bytes()); // Flags
                b.extend_from_slice(&0u32.to_le_bytes()); // AuxBufSize
                b
            },
        };
        let wait_resp = handle(wait_req, &state).await;
        assert_eq!(wait_resp.code, ResponseCode::Success);
        // Body layout: StatusCode(4)=0 · ErrorCode(4)=0 · EventPending(4)=1 ·
        // AuxBufSize(4)=0.
        let (_s, _h, _ct, wait_body) = wait_resp.render();
        let payload = &wait_body[4..];
        assert_eq!(payload.len(), 16);
        let event_pending = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
        assert_eq!(
            event_pending, 1,
            "EventPending must be 1 for a queued event"
        );

        // Post-wait Execute (empty body) drains the queued event into a
        // RopNotify. The payload begins with RopId 0x2A, 4-byte
        // NotificationHandle = the subscription's handle index (4),
        // ReturnValue=Success(4), LogonId=2, then NotificationData.
        let exec_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:2".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: Vec::new(),
        };
        let exec_resp = handle(exec_req, &state).await;
        assert_eq!(exec_resp.code, ResponseCode::Success);
        let (_s, _h, _ct, exec_body) = exec_resp.render();
        let epayload = &exec_body[4..];
        assert_eq!(
            epayload[0], 0x2A,
            "first response byte is RopNotify (ROP_NOTIFY)"
        );
        let notif_handle = u32::from_le_bytes([epayload[1], epayload[2], epayload[3], epayload[4]]);
        assert_eq!(
            notif_handle, 4,
            "NotificationHandle echoes the subscription index"
        );
        let rv = u32::from_le_bytes([epayload[5], epayload[6], epayload[7], epayload[8]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        assert_eq!(epayload[9], 2, "LogonId echoed");
        // NotificationData: Flags(2 LE)=0x8002 (NewMail + message bit).
        let flags = u16::from_le_bytes([epayload[10], epayload[11]]);
        assert_eq!(flags, 0x8002);

        // A SECOND Execute drains nothing (no RopPending, no RopNotify).
        let exec_req2 = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:3".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: Vec::new(),
        };
        let exec_resp2 = handle(exec_req2, &state).await;
        assert_eq!(exec_resp2.code, ResponseCode::Success);
        let (_s, _h, _ct, exec_body2) = exec_resp2.render();
        let epayload2 = &exec_body2[4..];
        assert!(
            epayload2.is_empty(),
            "no notifications queued after drain -> empty Execute body"
        );
    }

    /// End-to-end regression (PR #1847 review): an ObjectMoved `RopNotify` MUST
    /// carry the MANDATORY OldFolderId + OldMessageId bytes on the wire. The
    /// previous `build_notification_data` left both `None`, and the encoder
    /// omitted them, truncating the notification and desyncing downstream ROP
    /// parsing. This test drives the full NotificationWait->Execute path with an
    /// `ItemMoved` event and asserts the Old* fields are present.
    #[tokio::test]
    async fn execute_drains_moved_rop_notify_with_mandatory_old_fields() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let mgr = std::sync::Arc::new(crate::notifications::SubscriptionManager::new());
        let state = MapiState::with_subscription_manager(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
            mgr.clone(),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        // Whole-store, ObjectMoved subscription at handle index 7, logon id 1.
        state.sessions.notifications().register(
            sid,
            7,
            crate::mapi::session::MapiNotificationSink::new(
                "u@example.com".into(),
                crate::mapi::session::NT_OBJECT_MOVED,
                crate::mapi::session::NotificationScope::WholeStore,
                1,
                mgr.subscribe_raw(),
            ),
        );
        mgr.publish(crate::notifications::NotificationEvent::ItemMoved {
            owner: "u@example.com".into(),
            new_folder_id: "inbox".into(),
            new_item_id: "M-9".into(),
            old_folder_id: "trash".into(),
            old_item_id: "M-9".into(),
            change_key: String::new(),
        });

        // NotificationWait so the event is pumped into the sink's pending queue.
        let wait_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::NotificationWait),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: {
                let mut b = Vec::new();
                b.extend_from_slice(&0u32.to_le_bytes());
                b.extend_from_slice(&0u32.to_le_bytes());
                b
            },
        };
        let wait_resp = handle(wait_req, &state).await;
        let (_s, _h, _ct, wait_body) = wait_resp.render();
        let event_pending = u32::from_le_bytes([
            wait_body[4 + 8],
            wait_body[4 + 9],
            wait_body[4 + 10],
            wait_body[4 + 11],
        ]);
        assert_eq!(event_pending, 1, "moved event must pump to pending");

        // Execute drains the ObjectMoved RopNotify.
        let exec_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:2".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: Vec::new(),
        };
        let exec_resp = handle(exec_req, &state).await;
        let (_s, _h, _ct, exec_body) = exec_resp.render();
        let p = &exec_body[4..];
        assert_eq!(p[0], 0x2A, "RopNotify");
        assert_eq!(p[9], 1, "LogonId echoed");
        // NotificationData: Flags(2 LE)=0x8020 (ObjectMoved + 0x8000 message).
        let flags = u16::from_le_bytes([p[10], p[11]]);
        assert_eq!(flags, 0x8020, "ObjectMoved message-event flags");
        // Layout of `p`: RopNotify header (RopId(1)+NotificationHandle(4)+
        // ReturnValue(4)+LogonId(1)=10) + NotificationData (Flags(2)+
        // FolderId(8)+MessageId(8)+OldFolderId(8)+OldMessageId(8)=34) = 44.
        // (No ParentFolderId: 0x4000 clear and 0x8000 set excludes it.)
        assert_eq!(
            p.len(),
            10 + 2 + 8 + 8 + 8 + 8,
            "header + Flags + Ids + Old* fully present"
        );
        let folder_id = u64::from_le_bytes(p[12..20].try_into().unwrap());
        let message_id = u64::from_le_bytes(p[20..28].try_into().unwrap());
        let old_folder_id = u64::from_le_bytes(p[28..36].try_into().unwrap());
        let old_message_id = u64::from_le_bytes(p[36..44].try_into().unwrap());
        assert_eq!(
            folder_id,
            crate::mapi::store::folder_id_from_backend("inbox"),
            "FolderId = destination"
        );
        assert_eq!(
            message_id,
            crate::mapi::store::message_id_from_jmap("M-9"),
            "MessageId = destination item"
        );
        assert_eq!(
            old_folder_id,
            crate::mapi::store::folder_id_from_backend("trash"),
            "OldFolderId = source (mandatory, not omitted)"
        );
        assert_eq!(
            old_message_id,
            crate::mapi::store::message_id_from_jmap("M-9"),
            "OldMessageId = source item (mandatory, not omitted)"
        );
    }

    /// A `NotificationWait` for an UNKNOWN session returns the failure body
    /// (`StatusCode != 0`) so the client re-Connects. This is the no-session
    /// path (distinct from a live session with no sinks, covered below).
    #[tokio::test]
    async fn notification_wait_unknown_session_returns_failure_body() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let wait_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::NotificationWait),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: Vec::new(), // no session cookie
            body: Vec::new(),
        };
        let resp = handle(wait_req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_s, _h, _ct, body) = resp.render();
        let payload = &body[4..];
        // Failure body: StatusCode(4) != 0 · AuxBufSize(4)=0.
        assert_eq!(payload.len(), 8);
        let status = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        assert_ne!(status, 0, "unknown session -> StatusCode != 0");
    }

    /// A live session with NO registered sinks long-polls for the bounded
    /// `NOTIFICATION_WAIT_MAX` budget and then returns `EventPending=0`. With
    /// no `subscription_manager` the poll returns `false` immediately (nothing
    /// can ever fire); the test is run under `start_paused = true` so the
    /// bounded wait completes without wall-clock delay. A session WITH a
    /// manager but no sinks short-circuits to `false` immediately via the
    /// per-session `session_has_sinks` guard (also covered here when a manager
    /// is wired).
    #[tokio::test(start_paused = true)]
    async fn notification_wait_live_session_no_sinks_returns_event_pending_zero() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        // Wire a real SubscriptionManager so the code path reaches the
        // per-session no-sinks short-circuit (the manager-present branch). A
        // session with NO registered sinks must return EventPending=0 right away
        // rather than blocking for NOTIFICATION_WAIT_MAX.
        let mgr = std::sync::Arc::new(crate::notifications::SubscriptionManager::new());
        let state = MapiState::with_subscription_manager(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
            mgr,
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        let wait_req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::NotificationWait),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: None,
            username: None,
            password: None,
            cookies: vec![("MapiContext".into(), sid.to_string())],
            body: Vec::new(),
        };
        let started = std::time::Instant::now();
        let resp = handle(wait_req, &state).await;
        // With no sinks the per-session guard short-circuits to false
        // immediately — the call must NOT run the full NOTIFICATION_WAIT_MAX.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "no-sinks NotificationWait must short-circuit, not block"
        );
        assert_eq!(resp.code, ResponseCode::Success);
        let (_s, _h, _ct, body) = resp.render();
        let payload = &body[4..];
        // Success body: StatusCode(4)=0 · ErrorCode(4)=0 · EventPending(4)=0 ·
        // AuxBufSize(4)=0.
        assert_eq!(payload.len(), 16);
        let status = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        assert_eq!(status, 0, "known session -> StatusCode == 0");
        let event_pending = u32::from_le_bytes([payload[8], payload[9], payload[10], payload[11]]);
        assert_eq!(event_pending, 0, "no sinks -> EventPending == 0");
    }

    /// A `RopQueryRows` against a contents-table whose rows carry a cached
    /// `JmapEmail` source must materialise real `PR_SUBJECT` and `PR_MID`
    /// cells (not typed NULLs). This is the core Phase-2 wiring: the
    /// dispatcher's earlier stages produce bare row ids; the QueryRows arm
    /// consults the row's `source` to drive `store::email_to_cells`.
    #[tokio::test]
    async fn query_rows_materialises_email_subject_and_mid() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });

        // Build a JmapEmail with a known subject + JMAP id, then install a
        // Table handle (index 5) whose single row carries it as the source.
        let email = crate::jmap::JmapEmail {
            id: Some("M1".into()),
            subject: Some("Hello MAPI".into()),
            ..crate::jmap::JmapEmail::default()
        };
        let expected_mid = crate::mapi::store::message_id_from_jmap("M1");
        state.sessions.with_session_mut(&sid, |s| {
            let row = crate::mapi::session::TableRow {
                row_id: expected_mid,
                cells: Vec::new(),
                source: Some(
                    std::sync::Arc::new(email) as std::sync::Arc<dyn std::any::Any + Send + Sync>
                ),
            };
            s.set_handle(
                5,
                crate::mapi::session::Handle::Table {
                    kind: crate::mapi::session::FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: Vec::new(),
                    rows: vec![row],
                    cursor: 0,
                    total: 1,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });

        // Column set: PR_SUBJECT (PtypString=0x001F + id 0x0037) + PR_MID (PtypInteger64=0x0014 + id 0x6748).
        // MS-OXCDATA ┬¦2.9 PropertyTag wire order is PropertyType(2 LE) THEN PropertyId(2 LE).
        let set_columns_body: Vec<u8> = {
            let mut b = Vec::new();
            // RopId(0x06) ┬Ę LogonId(0) ┬Ę InputHandleIndex(5) ┬Ę SetColumnFlags(0)
            // ┬Ę PropertyTagCount(2 LE = 2) ┬Ę [tag1][tag2]
            b.extend_from_slice(&[0x06, 0, 5, 0, 2, 0]);
            b.extend_from_slice(
                &crate::mapi::data::PropertyType::PTYP_STRING
                    .to_u16()
                    .to_le_bytes(),
            );
            b.extend_from_slice(&0x0037u16.to_le_bytes()); // PR_SUBJECT id
            b.extend_from_slice(
                &crate::mapi::data::PropertyType::PTYP_INTEGER64
                    .to_u16()
                    .to_le_bytes(),
            );
            b.extend_from_slice(&0x6748u16.to_le_bytes()); // PR_MID id
            b
        };
        let mut cur = crate::mapi::rops::Buf::new(&set_columns_body);
        cur.take_u8().ok(); // consume RopId
        let mut out_sc = Vec::new();
        let snap = state.sessions.get(&sid).expect("session");
        execute_one_rop(
            crate::mapi::rops::RopId::ROP_SET_COLUMNS,
            &mut cur,
            &mut out_sc,
            &sid,
            &state.sessions,
            &snap,
            None,
            &Config::default(),
            "u@example.com",
            None,
            0,
            None,
            None,
        )
        .await
        .expect("set_columns dispatch");

        // QueryRows: RopId(0x15) ┬Ę LogonId(0) ┬Ę InputHandleIndex(5) ┬Ę
        // QueryRowsFlags(0) ┬Ę ForwardRead(0) ┬Ę RowCount(2 LE = 1)
        let qr_body: Vec<u8> = vec![0x15, 0, 5, 0, 0, 1, 0];
        let mut cur2 = crate::mapi::rops::Buf::new(&qr_body);
        cur2.take_u8().ok(); // consume RopId
        let mut out_qr = Vec::new();
        let snap2 = state.sessions.get(&sid).expect("session");
        execute_one_rop(
            crate::mapi::rops::RopId::ROP_QUERY_ROWS,
            &mut cur2,
            &mut out_qr,
            &sid,
            &state.sessions,
            &snap2,
            None,
            &Config::default(),
            "u@example.com",
            None,
            0,
            None,
            None,
        )
        .await
        .expect("query_rows dispatch");

        // Response: RopId(0x15) ┬Ę InputHandleIndex(5) ┬Ę ReturnValue(4 LE=0)
        // ┬Ę Origin(1) ┬Ę RowCount(2 LE=1) ┬Ę flag(1)=0 ┬Ę <subject cell> ┬Ę <mid cell>
        assert_eq!(out_qr[0], 0x15);
        assert_eq!(out_qr[1], 5);
        let rv = u32::from_le_bytes([out_qr[2], out_qr[3], out_qr[4], out_qr[5]]);
        assert_eq!(
            crate::mapi::rops::RopErrorCode::from_u32(rv),
            crate::mapi::rops::RopErrorCode::Success
        );
        // off 6 = Origin; off 7..9 = RowCount.
        let row_count = u16::from_le_bytes([out_qr[7], out_qr[8]]);
        assert_eq!(row_count, 1, "row_count");
        // First byte of row data is the StandardPropertyRow flag (0).
        assert_eq!(out_qr[9], 0u8, "row flag");
        // PR_SUBJECT cell per MS-OXCDATA ┬¦2.11.2.1: UTF-16LE code units
        // INCLUDING the 0x0000 terminator, with NO length prefix.
        // "Hello MAPI" is 10 code units ŌåÆ 22 bytes (no length word).
        let subj_u16: Vec<u16> = (0..10)
            .map(|i| u16::from_le_bytes([out_qr[10 + 2 * i], out_qr[11 + 2 * i]]))
            .collect();
        let subj = String::from_utf16_lossy(&subj_u16);
        assert_eq!(subj, "Hello MAPI");
        // Terminating NUL (0x0000) at out_qr[30..32].
        assert_eq!(
            u16::from_le_bytes([out_qr[30], out_qr[31]]),
            0,
            "subject NUL"
        );
        // PR_MID cell: 8-byte LE Integer64 == expected_mid immediately after.
        let mid_off = 10 + 10 * 2 + 2; // skip body(20) + NUL(2)
        let packed = i64::from_le_bytes([
            out_qr[mid_off],
            out_qr[mid_off + 1],
            out_qr[mid_off + 2],
            out_qr[mid_off + 3],
            out_qr[mid_off + 4],
            out_qr[mid_off + 5],
            out_qr[mid_off + 6],
            out_qr[mid_off + 7],
        ]);
        assert_eq!(packed as u64, expected_mid, "PR_MID matches row id");
    }

    /// Regression: `RopOpenFolder` wire (`RopId(0x02)┬ĘLogonId┬ĘInputHandle┬ĘOutputHandle
    /// ┬ĘFolderId(8 LE)┬ĘOpenModeFlags(1)`) must reach the dispatcher's
    /// folder-open path with the **real** input/output-handle indices and not
    /// bytes shifted by one. A previous implementation called
    /// `RopHeader4::decode` after the dispatcher had already consumed the
    /// leading `RopId` byte, so `RopHeader4` re-read the LogonId as the RopId,
    /// the InputHandle as the LogonId, the OutputHandle as the InputHandle,
    /// and the high byte of FolderId as the OutputHandle ŌĆö corrupting both
    /// the output-handle index the response echoes and the folder resolution.
    #[tokio::test]
    async fn execute_rop_open_folder_handles_header_after_ropid() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const INPUT_HANDLE: u8 = 7;
        const OUTPUT_HANDLE: u8 = 11;
        const FOLDER_ID: u64 = 0x0123_4567_89AB_CDEF;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Folder {
                    backend_id: "I".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                },
            );
        });

        // Wire: RopId┬ĘLogonId┬ĘInput┬ĘOutput┬ĘFolderId(8 LE)┬ĘOpenModeFlags(1) = 13 bytes.
        let mut body = vec![0x02u8, /*LogonId*/ 0x00, INPUT_HANDLE, OUTPUT_HANDLE];
        body.extend_from_slice(&FOLDER_ID.to_le_bytes());
        body.push(0); // OpenModeFlags (Open mode = 0).

        let mut cur = crate::mapi::rops::Buf::new(&body);
        cur.take_u8().ok(); // consume RopId ŌĆö matches the runtime dispatcher.
        let mut out = Vec::new();
        let snap = state.sessions.get(&sid).expect("session");
        execute_one_rop(
            crate::mapi::rops::RopId::ROP_OPEN_FOLDER,
            &mut cur,
            &mut out,
            &sid,
            &state.sessions,
            &snap,
            None,
            &Config::default(),
            "u@example.com",
            None,
            0,
            None,
            None,
        )
        .await
        .expect("open folder dispatch");

        // RopOpenFolderSuccess: RopId(0x02) ┬Ę OutputHandleIndex ┬Ę ReturnValue(4 LE)
        // ┬Ę HasRules(1) ┬Ę IsGhosted(1).
        assert_eq!(out.len(), 8, "open_folder response length");
        assert_eq!(out[0], 0x02, "echoed RopId");
        assert_eq!(out[1], OUTPUT_HANDLE, "output handle index (NOT shifted)");
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::Success,
            "open_folder return value"
        );
        assert_eq!(out[6], 0, "has_rules");
        assert_eq!(out[7], 0, "is_ghosted");

        // The output handle must now point at the same folder the input
        // handle resolved to ("I"); a header-skew bug would have resolved it
        // from handle index == high byte of FolderId (i.e. index 0xEF), and
        // that handle doesn't exist on the session.
        let snap2 = state.sessions.get(&sid).expect("session2");
        match snap2.handles.get(&OUTPUT_HANDLE) {
            Some(crate::mapi::session::Handle::Folder { backend_id, .. }) => {
                assert_eq!(backend_id, "I", "open_folder installed output handle");
            }
            other => panic!("output handle not a Folder: {other:?}"),
        }
    }

    /// `RopCreateMessage` must install a `Message { is_new: true }` handle at the
    /// client's OutputHandleIndex and echo `RopId(0x06) ┬Ę OutputHandleIndex
    /// ┬Ę Success ┬Ę HasMessageId=1 ┬Ę MessageId(8 LE=0 placeholder)`, even with
    /// no JMAP backend configured ŌĆö the draft is not persisted until the
    /// subsequent `RopSaveChangesMessage`.
    #[tokio::test]
    async fn execute_rop_create_message_installs_new_message_handle() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const INPUT_HANDLE: u8 = 3;
        const OUTPUT_HANDLE: u8 = 9;
        const FOLDER_ID: u64 = 0x0123_4567_89AB_CDEF;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Folder {
                    backend_id: "drafts-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                },
            );
        });
        // Wire: RopId(0x06) ┬Ę LogonId(0) ┬Ę InputHandle(3) ┬Ę OutputHandle(9)
        // ┬Ę CodePageId(2 LE=0) ┬Ę FolderId(8 LE) ┬Ę AssociatedFlag(1=0) = 13 bytes.
        let mut body = vec![0x06u8, 0x00, INPUT_HANDLE, OUTPUT_HANDLE];
        body.extend_from_slice(&0u16.to_le_bytes()); // CodePageId
        body.extend_from_slice(&FOLDER_ID.to_le_bytes());
        body.push(0); // AssociatedFlag
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: None,
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        // RopCreateMessageSuccess: RopId ┬Ę OutputHandleIndex ┬Ę RV(4) ┬Ę
        // HasMessageId(1) ┬Ę MessageId(8 LE placeholder).
        assert_eq!(payload[0], 0x06, "echoed RopId");
        assert_eq!(payload[1], OUTPUT_HANDLE, "output handle (NOT shifted)");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        assert_eq!(payload[6], 1, "HasMessageId present");
        let mid = u64::from_le_bytes(payload[7..15].try_into().unwrap());
        assert_eq!(mid, 0, "placeholder MessageId until SaveChanges");
        // The OUTPUT handle is a new Message bound to the drafts mailbox.
        let snap = state.sessions.get(&sid).expect("session");
        match snap.handles.get(&OUTPUT_HANDLE) {
            Some(crate::mapi::session::Handle::Message {
                backend_id,
                mailbox_id,
                is_new,
                ..
            }) => {
                assert!(backend_id.is_empty(), "no backend id until save");
                assert_eq!(mailbox_id, "drafts-mbox");
                assert!(*is_new, "is_new until SaveChanges");
            }
            other => panic!("output handle not a new Message: {other:?}"),
        }
    }

    /// `RopSaveChangesMessage` without a JMAP backend must emit
    /// `RopNotFound` instead of a silent Success, and must NOT corrupt the
    /// session. This guards against the regression where the write path
    /// reported `Success` with no work done.
    #[tokio::test]
    async fn execute_rop_save_changes_message_without_backend_emits_not_found() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        // No jmap_base ŌåÆ jmap backend is None at dispatch time.
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const RESPONSE_HANDLE: u8 = 2;
        const INPUT_HANDLE: u8 = 4;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Message {
                    backend_id: String::new(),
                    mailbox_id: "drafts-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                    is_new: true,
                },
            );
        });
        // Wire: RopId(0x0C) ┬Ę LogonId(0) ┬Ę ResponseHandleIndex(2)
        // ┬Ę InputHandleIndex(4) ┬Ę SaveFlags(0).
        let body: Vec<u8> = vec![0x0C, 0, RESPONSE_HANDLE, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x0C, "echoed RopId");
        // RopSaveChangesMessageSuccess: RopId ┬Ę ResponseHandleIndex ┬Ę RV(4) ┬Ę
        // InputHandleIndex ┬Ę MessageId(8).
        assert_eq!(payload[1], RESPONSE_HANDLE, "response handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NotFound,
            "no JMAP backend ŌćÆ NotFound, not silent Success"
        );
        assert_eq!(payload[6], INPUT_HANDLE, "input handle echoed");
    }

    /// `RopSaveChangesMessage` with a dirty body stream on the message must
    /// surface `RopNoSupport` (not a silent Success) because the body write-back
    /// bridge is not yet wired; the client is told the save did not commit the
    /// staged bytes rather than being faked success.
    #[tokio::test]
    async fn execute_rop_save_changes_message_dirty_body_stream_emits_no_support() {
        let (state, sid) = state_with_session();
        const RESPONSE_HANDLE: u8 = 2;
        const INPUT_HANDLE: u8 = 4;
        const STREAM_HANDLE: u8 = 5;
        // A draft mail message the body-stream is owned by.
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Message {
                    backend_id: "m1".into(),
                    mailbox_id: "drafts-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                    is_new: true,
                },
            );
        });
        // A dirty body stream owned by the message handle (PR_BODY, wrote some
        // bytes, not read-only).
        let body_tag = crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING8,
            crate::mapi::store::PR_BODY,
        );
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                STREAM_HANDLE,
                crate::mapi::session::Handle::Stream {
                    source_handle_index: INPUT_HANDLE,
                    kind: crate::mapi::session::FolderKind::Mail,
                    backend_id: "m1".into(),
                    mailbox_id: "drafts-mbox".into(),
                    property_tag: body_tag,
                    data: Some(b"new body".to_vec()),
                    known_len: None,
                    cursor: 8,
                    is_dirty: true,
                    read_only: false,
                },
            );
        });
        // Wire: RopId(0x0C) - LogonId(0) - ResponseHandleIndex(2)
        // - InputHandleIndex(4) - SaveFlags(0).
        let body: Vec<u8> = vec![0x0C, 0, RESPONSE_HANDLE, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x0C, "echoed RopId");
        assert_eq!(payload[1], RESPONSE_HANDLE, "response handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NoSupport,
            "dirty body stream -> NoSupport, not silent Success"
        );
        assert_eq!(payload[6], INPUT_HANDLE, "input handle echoed");
    }

    /// `RopSaveChangesMessage` with NO dirty body stream still proceeds to the
    /// backend create path (and, with no JMAP backend configured, emits
    /// `NotFound` rather than `NoSupport`), proving the dirty-body guard does
    /// not over-trigger.
    #[tokio::test]
    async fn execute_rop_save_changes_message_clean_message_falls_through_to_backend() {
        let (state, sid) = state_with_session();
        const RESPONSE_HANDLE: u8 = 2;
        const INPUT_HANDLE: u8 = 4;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Message {
                    backend_id: "m1".into(),
                    mailbox_id: "drafts-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                    is_new: true,
                },
            );
        });
        let body: Vec<u8> = vec![0x0C, 0, RESPONSE_HANDLE, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        // No backend -> NotFound (not NoSupport): the dirty-body guard gated out.
        assert_ne!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NoSupport,
            "clean message must not trigger the dirty-body NoSupport path"
        );
    }

    /// `RopDeleteMessages` against a non-mail folder (e.g. a root handle)
    /// must yield `RopNoSupport` so the client reacts instead of a silent
    /// success, and the cursor must be advanced exactly past the message-id
    /// array so a chained ROP can resume.
    #[tokio::test]
    async fn execute_rop_delete_messages_non_mail_folder_emits_no_support() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const INPUT_HANDLE: u8 = 1;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Folder {
                    backend_id: "ROOT".into(),
                    kind: crate::mapi::session::FolderKind::Root,
                },
            );
        });
        // Wire: RopId(0x1E) ┬Ę LogonId(0) ┬Ę InputHandle(1) ┬Ę WantAsynchronous(0)
        // ┬Ę NotifyNonRead(0) ┬Ę MessageIdCount(2 LE=1) ┬Ę MessageId(8 LE=42).
        let mut body = vec![0x1Eu8, 0, INPUT_HANDLE, 0, 0];
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&42u64.to_le_bytes());
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x1E, "echoed RopId");
        assert_eq!(payload[1], INPUT_HANDLE, "input handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NoSupport,
            "non-mail folder ŌćÆ NoSupport"
        );
        assert_eq!(payload[6], 0, "PartialCompletion=0");
    }

    /// `RopSubmitMessage` against an unsaved (`is_new`) draft must emit
    /// `InvalidParameter` rather than attempting to submit a draft with no
    /// backend id ŌĆö guarding the EmailSubmission path against an envelope
    /// built from an empty/stale email object.
    #[tokio::test]
    async fn execute_rop_submit_message_unsaved_draft_emits_invalid_parameter() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const INPUT_HANDLE: u8 = 7;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Message {
                    backend_id: String::new(),
                    mailbox_id: "drafts-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                    is_new: true,
                },
            );
        });
        // Wire: RopId(0x32) ┬Ę LogonId(0) ┬Ę InputHandle(7) ┬Ę SubmitFlags(0).
        let body: Vec<u8> = vec![0x32, 0, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x32, "echoed RopId");
        assert_eq!(payload[1], INPUT_HANDLE, "input handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::InvalidParameter,
            "unsaved draft ŌćÆ InvalidParameter"
        );
    }

    /// `RopTransportSend` failure (no JMAP backend) must emit the FAILURE
    /// response shape `RopId ┬Ę InputHandleIndex ┬Ę ReturnValue(4)` ŌĆö NOT the
    /// success shape that adds `NoPropertiesReturned ┬Ę PropertyValueCount`.
    /// A regression earlier emitted the success envelope with a non-Success
    /// return value, which Outlook misparsed.
    #[tokio::test]
    async fn execute_rop_transport_send_failure_emits_failure_envelope() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const INPUT_HANDLE: u8 = 5;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                INPUT_HANDLE,
                crate::mapi::session::Handle::Message {
                    backend_id: "MABC".into(),
                    mailbox_id: "sent-mbox".into(),
                    kind: crate::mapi::session::FolderKind::Mail,
                    is_new: false,
                },
            );
        });
        // Wire: RopId(0x4A) ┬Ę LogonId(0) ┬Ę InputHandle(5).
        let body: Vec<u8> = vec![0x4A, 0, INPUT_HANDLE];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        // Failure envelope is exactly 6 bytes: RopId ┬Ę InputHandleIndex ┬Ę RV(4).
        assert_eq!(payload.len(), 6, "transport-send failure envelope length");
        assert_eq!(payload[0], 0x4A, "echoed RopId");
        assert_eq!(payload[1], INPUT_HANDLE, "input handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NotFound,
            "no JMAP backend ŌćÆ NotFound"
        );
    }

    /// `email_recipients` dedupes across to/cc/bcc and skips empty addresses.
    #[test]
    fn email_recipients_dedupes_and_skips_empty() {
        use crate::jmap::{JmapEmail, JmapEmailAddress};
        let e = JmapEmail {
            to: Some(vec![
                JmapEmailAddress {
                    name: None,
                    email: Some("a@x.com".into()),
                },
                JmapEmailAddress {
                    name: None,
                    email: Some("b@x.com".into()),
                },
            ]),
            cc: Some(vec![
                JmapEmailAddress {
                    name: None,
                    email: Some("a@x.com".into()),
                }, // dup of to
                JmapEmailAddress {
                    name: None,
                    email: Some("".into()),
                }, // empty, skipped
            ]),
            bcc: Some(vec![JmapEmailAddress {
                name: None,
                email: Some("c@x.com".into()),
            }]),
            ..Default::default()
        };
        let rcpts = super::email_recipients(&e);
        assert_eq!(rcpts, vec!["a@x.com", "b@x.com", "c@x.com"]);
    }

    /// A `RopMoveCopyMessages` request against two non-mail handles must
    /// emit `NoSupport` and echo the SOURCE handle index (per ┬¦2.2.4.6.2 the
    /// response's first byte after RopId is the SourceHandleIndex, NOT the
    /// dest).
    #[tokio::test]
    async fn execute_rop_move_copy_messages_non_mail_emits_no_support() {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        const SRC_HANDLE: u8 = 2;
        const DEST_HANDLE: u8 = 8;
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                SRC_HANDLE,
                crate::mapi::session::Handle::Folder {
                    backend_id: "ROOT".into(),
                    kind: crate::mapi::session::FolderKind::Root,
                },
            );
            s.set_handle(
                DEST_HANDLE,
                crate::mapi::session::Handle::Folder {
                    backend_id: "ROOT".into(),
                    kind: crate::mapi::session::FolderKind::Root,
                },
            );
        });
        // Wire: RopId(0x33) ┬Ę LogonId(0) ┬Ę SourceHandle(2) ┬Ę DestHandle(8)
        // ┬Ę MessageIdCount(2 LE=1) ┬Ę MessageId(8 LE=7) ┬Ę WantAsync(0) ┬Ę WantCopy(0).
        let mut body = vec![0x33u8, 0, SRC_HANDLE, DEST_HANDLE];
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&7u64.to_le_bytes());
        body.push(0); // WantAsynchronous
        body.push(0); // WantCopy (move)
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            username: None,
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x33, "echoed RopId");
        assert_eq!(payload[1], SRC_HANDLE, "SOURCE handle echoed (not dest)");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NoSupport);
        assert_eq!(payload[6], 0, "PartialCompletion=0");
    }

    #[test]
    fn outcome_to_code_maps_success_to_success() {
        let o = crate::jmap::EmailSetOutcome {
            updated: vec!["M-1".to_string()],
            not_updated: Vec::new(),
            method_error: None,
        };
        assert_eq!(outcome_to_code(o, "test"), RopErrorCode::Success);
    }

    #[test]
    fn outcome_to_code_maps_per_id_rejection_to_disk_error() {
        // A `notUpdated` entry used to be masked as Success because the prior
        // handler called `update_email` (which returned `Ok(())`) and mapped
        // every `Ok` to `Success`. The new `update_email_checked` path
        // surfaces the rejection and `outcome_to_code` MUST turn it into a
        // MAPI DiskError so Outlook surfaces it (qodo #3/#5, cubic #23).
        let o = crate::jmap::EmailSetOutcome {
            updated: Vec::new(),
            not_updated: vec![("M-2".to_string(), "notFound".to_string())],
            method_error: None,
        };
        assert_eq!(outcome_to_code(o, "test"), RopErrorCode::DiskError);
    }

    #[test]
    fn outcome_to_code_maps_method_error_to_disk_error() {
        let o = crate::jmap::EmailSetOutcome {
            updated: Vec::new(),
            not_updated: Vec::new(),
            method_error: Some("serverFail".to_string()),
        };
        assert_eq!(outcome_to_code(o, "test"), RopErrorCode::DiskError);
    }

    #[test]
    fn publish_item_modified_noop_without_manager() {
        // Without a wired SubscriptionManager (the unit-test fixture path),
        // publishing MUST be a safe no-op ŌĆö verifies the `Option` plumbing
        // doesn't unwrap-and-panic and that the helper short-circuits cleanly
        // (qodo #9, cubic #30). (A live-manager variant needs a Tokio
        // runtime to construct the broadcast channel and so is covered by
        // the integration test target instead.)
        publish_item_modified(None, "u@example.com", "f", "M-1");
    }

    // ---- Stream ROP dispatcher coverage ------------------------------------
    //
    // The OpenStream arm resolves a body/attachment against JMAP; with no
    // JMAP client wired it returns `NotFound`/`NoSupport`. The remaining six
    // stream arms operate purely on a pre-installed `Handle::Stream` buffer,
    // so they are exercised here without any backend round-trip.

    fn state_with_session() -> (MapiState, uuid::Uuid) {
        let mut cfg = Config::test_with_mail_domain("example.com");
        cfg.mapi_enabled = true;
        let state = MapiState::new(
            cfg,
            std::sync::Arc::new(AuthVerifier::new(&Config::default())),
        );
        let sid = state
            .sessions
            .create(crate::mapi::session::SessionPrincipal {
                email: "u@example.com".into(),
                basic_auth: true,
            });
        (state, sid)
    }

    /// Install a Stream handle at `idx` seeded with `bytes` and the cursor at
    /// `cursor`. `read_only=true` models an attachment blob stream.
    fn install_stream(
        state: &MapiState,
        sid: &uuid::Uuid,
        idx: u8,
        bytes: Vec<u8>,
        cursor: u64,
        read_only: bool,
    ) {
        let property_tag = crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING8,
            crate::mapi::store::PR_BODY,
        );
        state.sessions.with_session_mut(sid, |s| {
            s.set_handle(
                idx,
                Handle::Stream {
                    source_handle_index: 0,
                    kind: FolderKind::Mail,
                    backend_id: String::new(),
                    mailbox_id: String::new(),
                    property_tag,
                    data: Some(bytes),
                    known_len: None,
                    cursor,
                    is_dirty: false,
                    read_only,
                },
            );
        });
    }

    /// Drive one ROP through `execute_one_rop`, returning its response bytes.
    async fn dispatch(state: &MapiState, sid: &uuid::Uuid, body: &[u8]) -> Vec<u8> {
        let mut cur = Buf::new(body);
        cur.take_u8().ok(); // consume RopId (dispatcher convention)
        let mut out = Vec::new();
        let snap = state.sessions.get(sid).expect("session");
        execute_one_rop(
            RopId::from_u8(body[0]),
            &mut cur,
            &mut out,
            sid,
            &state.sessions,
            &snap,
            None,
            &Config::default(),
            "u@example.com",
            None,
            0,
            None,
            None,
        )
        .await
        .expect("dispatch ok");
        out
    }

    #[tokio::test]
    async fn read_stream_pages_and_advances_cursor() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 4, b"Hello, MAPI world".to_vec(), 0, false);
        // ReadStream: RopId(0x2C) ┬Ę LogonId(0) ┬Ę InputHandleIndex(4) ┬Ę ByteCount(5)
        let out = dispatch(&state, &sid, &[0x2C, 0, 4, 5, 0]).await;
        assert_eq!(out[0], 0x2C);
        assert_eq!(out[1], 4);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        let data_size = u16::from_le_bytes([out[6], out[7]]);
        assert_eq!(data_size, 5);
        assert_eq!(&out[8..13], b"Hello");
        // Cursor now at 5: a second read returns the next 5 bytes.
        let out2 = dispatch(&state, &sid, &[0x2C, 0, 4, 5, 0]).await;
        assert_eq!(&out2[8..13], b", MAP");
        // Final read returns the remainder("I world").
        let out3 = dispatch(&state, &sid, &[0x2C, 0, 4, 0xFF, 0xFF]).await;
        let ds3 = u16::from_le_bytes([out3[6], out3[7]]);
        let rest = String::from_utf8_lossy(&out3[8..8 + ds3 as usize]);
        assert_eq!(rest, "I world");
    }

    #[tokio::test]
    async fn seek_stream_repositions_cursor() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 1, b"abcdefghij".to_vec(), 0, false);
        // SeekStream: RopId(0x2E) ┬Ę LogonId(0) ┬Ę InputHandleIndex(1) ┬Ę
        // Origin(0x00=begin) ┬Ę Offset(8 LE = 3)
        let mut body: Vec<u8> = vec![0x2E, 0, 1, 0x00];
        body.extend_from_slice(&3u64.to_le_bytes());
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x2E);
        let new_pos = u64::from_le_bytes([
            out[6], out[7], out[8], out[9], out[10], out[11], out[12], out[13],
        ]);
        assert_eq!(new_pos, 3);
        // Reading 4 bytes from position 3 returns "defg".
        let outrs = dispatch(&state, &sid, &[0x2C, 0, 1, 4, 0]).await;
        assert_eq!(&outrs[8..12], b"defg");
    }

    #[tokio::test]
    async fn get_stream_size_reports_buffer_length() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 2, b"0123456789".to_vec(), 0, false);
        // GetStreamSize: RopId(0x5E) ┬Ę LogonId(0) ┬Ę InputHandleIndex(2)
        let out = dispatch(&state, &sid, &[0x5E, 0, 2]).await;
        assert_eq!(out[0], 0x5E);
        let size = u32::from_le_bytes([out[6], out[7], out[8], out[9]]);
        assert_eq!(size, 10);
    }

    #[tokio::test]
    async fn write_stream_overwrites_and_extends() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 3, b"hello".to_vec(), 0, false);
        // WriteStream "XY" at cursor 0 ŌåÆ "XYllo", cursor=2.
        let mut body: Vec<u8> = vec![0x2D, 0, 3, 2, 0];
        body.extend_from_slice(b"XY");
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x2D);
        assert_eq!(u16::from_le_bytes([out[6], out[7]]), 2);
        // Write "Z" at cursor 2 ŌåÆ "XYZlo".
        let mut body2: Vec<u8> = vec![0x2D, 0, 3, 1, 0];
        body2.extend_from_slice(b"Z");
        dispatch(&state, &sid, &body2).await;
        // SeekStream back to 0 before reading the whole buffer (the cursor is
        // at 3 after the second write).
        let mut seek_body: Vec<u8> = vec![0x2E, 0, 3, 0x00];
        seek_body.extend_from_slice(&0u64.to_le_bytes());
        dispatch(&state, &sid, &seek_body).await;
        // GetStreamSize == 5, then read the full 5 bytes.
        let outgss = dispatch(&state, &sid, &[0x5E, 0, 3]).await;
        assert_eq!(
            u32::from_le_bytes([outgss[6], outgss[7], outgss[8], outgss[9]]),
            5
        );
        let outrs = dispatch(&state, &sid, &[0x2C, 0, 3, 0xFF, 0xFF]).await;
        let ds = u16::from_le_bytes([outrs[6], outrs[7]]);
        assert_eq!(String::from_utf8_lossy(&outrs[8..8 + ds as usize]), "XYZlo");
    }

    #[tokio::test]
    async fn set_stream_size_truncates_and_clamps_cursor() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 5, b"abcdefghij".to_vec(), 9, false);
        // SetStreamSize: RopId(0x2F) ┬Ę LogonId(0) ┬Ę InputHandleIndex(5) ┬Ę StreamSize(8=4)
        let mut body: Vec<u8> = vec![0x2F, 0, 5];
        body.extend_from_slice(&4u64.to_le_bytes());
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x2F);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        // The buffer is now 4 bytes and the cursor clamped to 4.
        let snap = state.sessions.get(&sid).expect("session");
        match snap.handles.get(&5).unwrap() {
            Handle::Stream { data, cursor, .. } => {
                assert_eq!(data.as_ref().unwrap().len(), 4);
                assert_eq!(*cursor, 4);
            }
            _ => panic!("expected stream handle"),
        }
    }

    #[tokio::test]
    async fn set_stream_size_rejects_oversize() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 5, b"ab".to_vec(), 0, false);
        // A size over the spec ceiling (2^31) AND the configured per-stream
        // cap is rejected; the per-stream cap maps the rejection to
        // NotEnoughMemory (memory pressure), not InvalidParameter.
        let mut body: Vec<u8> = vec![0x2F, 0, 5];
        body.extend_from_slice(&0x8000_0001u64.to_le_bytes());
        let out = dispatch(&state, &sid, &body).await;
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NotEnoughMemory);
    }

    #[tokio::test]
    async fn write_stream_on_readonly_attachment_is_access_denied() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 6, b"blob".to_vec(), 0, true);
        let mut body: Vec<u8> = vec![0x2D, 0, 6, 1, 0];
        body.push(0x5A);
        let out = dispatch(&state, &sid, &body).await;
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::AccessDenied);
        // On a non-success ReturnValue the failure envelope is the 6-byte
        // RopId + InputHandleIndex + ReturnValue: NO WrittenSize (MS-OXCROPS
        // 2.2.9.3.3), so the response is exactly 6 bytes.
        assert_eq!(out.len(), 6);
    }

    #[tokio::test]
    async fn commit_stream_acks_bound_handle() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 8, b"body".to_vec(), 0, false);
        let out = dispatch(&state, &sid, &[0x5D, 0, 8]).await;
        assert_eq!(out[0], 0x5D);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
    }

    #[tokio::test]
    async fn commit_stream_unbound_handle_is_not_found() {
        let (state, sid) = state_with_session();
        let out = dispatch(&state, &sid, &[0x5D, 0, 9]).await;
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NotFound);
    }

    #[tokio::test]
    async fn open_stream_without_jmap_reports_not_found() {
        // A Mail Message handle is installed so the OpenStream guard passes;
        // with no JMAP client the backend resolve fails with `NotFound`.
        let (state, sid) = state_with_session();
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                1,
                Handle::Message {
                    backend_id: "M-1".into(),
                    mailbox_id: "I".into(),
                    kind: FolderKind::Mail,
                    is_new: false,
                },
            );
        });
        let mut body: Vec<u8> = vec![0x2B, 0, 1, 7];
        crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING8,
            crate::mapi::store::PR_BODY,
        )
        .encode(&mut body);
        body.push(0x00);
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x2B);
        assert_eq!(out[1], 7);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NotFound);
    }

    #[tokio::test]
    async fn open_stream_on_non_message_handle_is_no_support() {
        let (state, sid) = state_with_session();
        // A Table handle is not streamable.
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                1,
                Handle::Table {
                    kind: FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: Vec::new(),
                    rows: Vec::new(),
                    cursor: 0,
                    total: 0,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });
        let mut body: Vec<u8> = vec![0x2B, 0, 1, 9];
        crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING8,
            crate::mapi::store::PR_BODY,
        )
        .encode(&mut body);
        body.push(0x00);
        let out = dispatch(&state, &sid, &body).await;
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NoSupport);
    }

    /// Install a JMAP-native `Handle::Attachment` at `idx` carrying `blob_id`
    /// and a declared `size` (as captured by `RopOpenAttachment`).
    fn install_attachment(
        state: &MapiState,
        sid: &uuid::Uuid,
        idx: u8,
        blob_id: &str,
        size: Option<u64>,
    ) {
        state.sessions.with_session_mut(sid, |s| {
            s.set_handle(
                idx,
                Handle::Attachment {
                    email_id: "M-att".into(),
                    mailbox_id: "I".into(),
                    kind: FolderKind::Mail,
                    attach_num: 0,
                    blob_id: blob_id.into(),
                    name: "doc.pdf".into(),
                    content_type: "application/pdf".into(),
                    size,
                    is_new: false,
                },
            );
        });
    }

    /// Build the `RopOpenStream` body for a given property tag.
    fn open_stream_body(input: u8, output: u8, prop_id: u16, prop_type: u16) -> Vec<u8> {
        let mut body: Vec<u8> = vec![0x2B, 0, input, output];
        crate::mapi::data::PropertyTag::new(crate::mapi::data::PropertyType(prop_type), prop_id)
            .encode(&mut body);
        body.push(0x00); // OpenModeFlags
        body
    }

    #[tokio::test]
    async fn open_stream_on_attachment_packs_email_blob_and_reports_size() {
        // The Attachment-handle OpenStream fast path must:
        //   * install a read-only Stream whose `backend_id` is
        //     `<emailId>\x1F<blobId>` (never the email id alone),
        //   * report the JMAP-declared `size` as the initial `StreamSize`,
        //   * only fire for `PR_ATTACH_DATA_BIN`/`PTYP_BINARY`.
        let (state, sid) = state_with_session();
        install_attachment(&state, &sid, 1, "blob-xyz", Some(4096));
        let out = dispatch(
            &state,
            &sid,
            &open_stream_body(
                1,
                7,
                crate::mapi::store::PR_ATTACH_DATA_BIN,
                crate::mapi::data::PropertyType::PTYP_BINARY.0,
            ),
        )
        .await;
        assert_eq!(out[0], 0x2B);
        assert_eq!(out[1], 7); // OutputHandleIndex
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        let stream_size = u32::from_le_bytes([out[6], out[7], out[8], out[9]]);
        assert_eq!(stream_size, 4096); // from the handle's captured size
        // The installed Stream handle carries the packed email#blob id and is
        // read-only.
        let snap = state.sessions.get(&sid).expect("session");
        match snap.handles.get(&7).unwrap() {
            Handle::Stream {
                backend_id,
                read_only,
                known_len,
                ..
            } => {
                assert_eq!(backend_id, "M-att\u{1F}blob-xyz");
                assert!(*read_only);
                assert_eq!(*known_len, Some(4096));
            }
            _ => panic!("expected stream handle"),
        }
    }

    #[tokio::test]
    async fn open_stream_on_attachment_non_data_bin_falls_through() {
        // A metadata property (`PR_ATTACH_LONG_FILENAME`) on an Attachment
        // handle must NOT receive the binary blob ŌĆö the fast path is gated to
        // `PR_ATTACH_DATA_BIN`/`PTYP_BINARY`, so the request falls through to
        // the legacy message-scoped path. There the Attachment handle maps to
        // a non-Mail source kind, so the `NoSupport` guard fires (the critical
        // assertion: no attachment bytes are returned for a metadata query and
        // no Stream handle is installed).
        let (state, sid) = state_with_session();
        install_attachment(&state, &sid, 1, "blob-xyz", Some(4096));
        let out = dispatch(
            &state,
            &sid,
            &open_stream_body(
                1,
                7,
                crate::mapi::store::PR_ATTACH_LONG_FILENAME,
                crate::mapi::data::PropertyType::PTYP_STRING.0,
            ),
        )
        .await;
        assert_eq!(out[0], 0x2B);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NoSupport);
        // No Stream handle was installed at the output index.
        let snap = state.sessions.get(&sid).expect("session");
        assert!(!snap.handles.contains_key(&7));
    }

    #[tokio::test]
    async fn open_stream_on_attachment_oversize_is_not_enough_memory() {
        // A declared size over `max_attachment_bytes` is rejected with
        // `NotEnoughMemory` (a RopErrorResponse, not the success shape)
        // before any download ŌĆö matching the message-scoped path.
        let (state, sid) = state_with_session();
        // `Config::default()` caps attachments well below u32::MAX; a 256 MiB
        // attachment exceeds the default ceiling.
        install_attachment(&state, &sid, 1, "blob-big", Some(256 * 1024 * 1024));
        let out = dispatch(
            &state,
            &sid,
            &open_stream_body(
                1,
                7,
                crate::mapi::store::PR_ATTACH_DATA_BIN,
                crate::mapi::data::PropertyType::PTYP_BINARY.0,
            ),
        )
        .await;
        assert_eq!(out[0], 0x2B);
        // RopErrorResponse envelope: RopId ┬Ę OutputHandleIndex ┬Ę ReturnValue(4)
        // (no StreamSize tail on a non-Success).
        assert_eq!(out[1], 7);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::NotEnoughMemory);
        assert_eq!(out.len(), 6);
        let snap = state.sessions.get(&sid).expect("session");
        assert!(!snap.handles.contains_key(&7));
    }

    // ---- FastTransfer / table-nav dispatch coverage -----------------------

    /// Install a contents Table handle at `idx` with a single cached-subject
    /// row, mirroring `query_rows_materialises_email_subject_and_mid` but with
    /// a fixed-column-set so the FXICS source serialiser has live cells.
    fn install_mail_table_with_subject(state: &MapiState, sid: &uuid::Uuid, idx: u8) {
        let email = crate::jmap::JmapEmail {
            id: Some("M1".into()),
            subject: Some("Hello MAPI".into()),
            ..crate::jmap::JmapEmail::default()
        };
        let expected_mid = crate::mapi::store::message_id_from_jmap("M1");
        // Pre-materialise cells against the chosen column set so the source
        // builder is exercised without needing a SetColumns round-trip first.
        let cs = vec![
            crate::mapi::data::PropertyTag::new(
                crate::mapi::data::PropertyType::PTYP_STRING,
                0x0037, // PR_SUBJECT
            ),
            crate::mapi::data::PropertyTag::new(
                crate::mapi::data::PropertyType::PTYP_INTEGER64,
                0x6748, // PR_MID
            ),
        ];
        let cells = crate::mapi::store::email_to_cells(&email, &cs, FolderKind::Mail, "I");
        state.sessions.with_session_mut(sid, |s| {
            let row = crate::mapi::session::TableRow {
                row_id: expected_mid,
                cells,
                source: Some(
                    std::sync::Arc::new(email) as std::sync::Arc<dyn std::any::Any + Send + Sync>
                ),
            };
            s.set_handle(
                idx,
                crate::mapi::session::Handle::Table {
                    kind: FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: cs,
                    rows: vec![row],
                    cursor: 0,
                    total: 1,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });
    }

    #[tokio::test]
    async fn fast_transfer_source_copy_messages_then_get_buffer() {
        let (state, sid) = state_with_session();
        install_mail_table_with_subject(&state, &sid, 5);

        // RopFastTransferSourceCopyMessages: RopId(0x4B) ┬Ę LogonId(0) ┬Ę
        // InHandle(5) ┬Ę OutHandle(6) ┬Ę Flags(0) ┬Ę MessageIdCount(2 LE=0).
        let copy_body: Vec<u8> = vec![0x4B, 0, 5, 6, 0, 0, 0];
        let out = dispatch(&state, &sid, &copy_body).await;
        assert_eq!(out[0], 0x4B);
        assert_eq!(out[1], 6); // OutputHandleIndex
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);
        assert_eq!(out.len(), 6);

        // The source handle (6) should now carry a non-empty ICS buffer
        // beginning with the IncrSyncChg marker (0x40120003 LE ŌåÆ 03 00 12 40).
        let snap = state.sessions.get(&sid).expect("session");
        match snap.handles.get(&6).expect("source installed") {
            crate::mapi::session::Handle::FastTransferSource { buffer, done, .. } => {
                assert!(!buffer.is_empty());
                assert_eq!(&buffer[0..4], &0x40120003u32.to_le_bytes());
                assert!(!*done);
            }
            other => panic!("expected FastTransferSource, got {other:?}"),
        }

        // GetBuffer: RopId(0x4E) ┬Ę LogonId(0) ┬Ę InHandle(6) ┬Ę BufferSize(2 LE)
        // ┬Ę TransferFlags(0). Use a big buffer so a single GetBuffer returns
        // the whole stream in one shot and signals TransferStatus=Done (3).
        let gb = vec![0x4E, 0, 6, 0xFF, 0xFF, 0];
        let out = dispatch(&state, &sid, &gb).await;
        assert_eq!(out[0], 0x4E);
        // TransferStatus is a 2-byte LE field after the 4-byte ReturnValue.
        let status = u16::from_le_bytes([out[6], out[7]]);
        assert_eq!(status, 3, "expected Done in one shot");
        // Fixed wire layout (MS-OXCFXICS ┬¦3.2.6.4):
        // RopId(1) InHandle(1) RV(4) TransferStatus(2 LE) InProgressCount(2)
        // TotalStepCount(2) Reserved(1) TransferBufferSize(2 LE) Data.
        let size = u16::from_le_bytes([out[13], out[14]]) as usize;
        let data = out[15..15 + size].to_vec();
        assert!(!data.is_empty(), "expected non-empty first chunk");
        // The stream must terminate with the IncrSyncEnd marker
        // (0x40140003 LE -> 03 00 14 40).
        let end = 0x40140003u32.to_le_bytes();
        assert!(
            data.windows(4).any(|w| w == end),
            "stream must end with IncrSyncEnd"
        );
    }

    #[tokio::test]
    async fn restrict_and_query_position_filter_table() {
        let (state, sid) = state_with_session();
        // Install a table with TWO rows whose PR_IMPORTANCE (Integer32
        // id 0x0017) differ (1 and 5). Restrict to importance==5 and verify
        // QueryPosition reflects the filtered count (denominator=1).
        let cs = vec![crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_INTEGER32,
            0x0017, // PR_IMPORTANCE
        )];
        let mk_row = |imp: i32| crate::mapi::session::TableRow {
            row_id: imp as u64,
            cells: vec![crate::mapi::data::PropertyValue::Integer32(imp)],
            source: None,
        };
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                7,
                crate::mapi::session::Handle::Table {
                    kind: FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: cs.clone(),
                    rows: vec![mk_row(1), mk_row(5)],
                    cursor: 0,
                    total: 2,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });

        // RopRestrict: RopId(0x14) ┬Ę LogonId(0) ┬Ę InHandle(7) ┬Ę RestrictFlags(0)
        // ┬Ę RestrictionDataSize(2 LE) ┬Ę SRestriction: Property(0x04) ┬Ę
        // RelOp EQ(2) ┬Ę Tag(type=0x0003,id=0x0017) ┬Ę PropertyValue row-form
        // Integer32(4 bytes)=5.
        let mut rdata = vec![0x04, 2];
        rdata.extend_from_slice(&0x0003u16.to_le_bytes());
        rdata.extend_from_slice(&0x0017u16.to_le_bytes());
        rdata.extend_from_slice(&5i32.to_le_bytes());
        let mut body = vec![0x14, 0, 7, 0];
        body.extend_from_slice(&u16::try_from(rdata.len()).unwrap().to_le_bytes());
        body.extend_from_slice(&rdata);
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x14);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);

        // QueryPosition: RopId(0x17) ┬Ę LogonId(0) ┬Ę InHandle(7).
        let qp = dispatch(&state, &sid, &[0x17, 0, 7]).await;
        let num = u32::from_le_bytes([qp[6], qp[7], qp[8], qp[9]]);
        let den = u32::from_le_bytes([qp[10], qp[11], qp[12], qp[13]]);
        assert_eq!(den, 1, "filtered table should have 1 row");
        assert_eq!(num, 0);

        // QueryRows serves only the filtered row (the importance==5 one).
        let qr = dispatch(&state, &sid, &[0x15, 0, 7, 0, 0, 10, 0]).await;
        let rc = u16::from_le_bytes([qr[7], qr[8]]);
        assert_eq!(rc, 1, "only the restricted row should be served");
    }

    #[tokio::test]
    async fn seek_row_advances_and_clamps_cursor() {
        let (state, sid) = state_with_session();
        let cs = vec![crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_INTEGER32,
            0x0017,
        )];
        let mk_row = |i: u64| crate::mapi::session::TableRow {
            row_id: i,
            cells: vec![crate::mapi::data::PropertyValue::Integer32(i as i32)],
            source: None,
        };
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                8,
                crate::mapi::session::Handle::Table {
                    kind: FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: cs.clone(),
                    rows: vec![mk_row(1), mk_row(2), mk_row(3)],
                    cursor: 0,
                    total: 3,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });

        // SeekRow: RopId(0x18) ┬Ę LogonId(0) ┬Ę InHandle(8) ┬Ę SeekFlags(0) ┬Ę
        // RowCount(4 LE = 2). Cursor should be 2 after.
        let mut body = vec![0x18, 0, 8, 0];
        body.extend_from_slice(&2i32.to_le_bytes());
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x18);
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);

        // QueryPosition should now report numerator=2, denominator=3.
        let qp = dispatch(&state, &sid, &[0x17, 0, 8]).await;
        let num = u32::from_le_bytes([qp[6], qp[7], qp[8], qp[9]]);
        let den = u32::from_le_bytes([qp[10], qp[11], qp[12], qp[13]]);
        assert_eq!((num, den), (2, 3));

        // Seek past the end: RowCount=10 should clamp cursor at 3 and report
        // has_sought_less=1.
        let mut body2 = vec![0x18, 0, 8, 0];
        body2.extend_from_slice(&10i32.to_le_bytes());
        let out2 = dispatch(&state, &sid, &body2).await;
        assert_eq!(out2[6], 1, "has_sought_less when clamped");
    }

    #[tokio::test]
    async fn sort_table_reorders_rows_descending() {
        let (state, sid) = state_with_session();
        let tag = crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_INTEGER32,
            0x0017, // PR_IMPORTANCE
        );
        let cs = vec![tag];
        let mk_row = |i: i32| crate::mapi::session::TableRow {
            row_id: i as u64,
            cells: vec![crate::mapi::data::PropertyValue::Integer32(i)],
            source: None,
        };
        state.sessions.with_session_mut(&sid, |s| {
            s.set_handle(
                9,
                crate::mapi::session::Handle::Table {
                    kind: FolderKind::Mail,
                    parent_handle: -1,
                    parent_backend_id: "I".into(),
                    column_set: cs.clone(),
                    rows: vec![mk_row(1), mk_row(5), mk_row(3)],
                    cursor: 0,
                    total: 3,
                    restriction: crate::mapi::restrict::SRestriction::default(),
                    sort_orders: Vec::new(),
                    next_bookmark: 0,
                },
            );
        });

        // SortTable: RopId(0x13) ┬Ę LogonId(0) ┬Ę InHandle(9) ┬Ę SortFlags(0) ┬Ę
        // SortCount(2 LE=1) ┬Ę SortOrder[0]: SortFlags(0x01=desc) ┬Ę Tag.
        let mut body = vec![0x13, 0, 9, 0, 1, 0, 0x01];
        let mut tagb = Vec::new();
        tag.encode(&mut tagb);
        body.extend_from_slice(&tagb);
        let out = dispatch(&state, &sid, &body).await;
        let rv = u32::from_le_bytes([out[2], out[3], out[4], out[5]]);
        assert_eq!(RopErrorCode::from_u32(rv), RopErrorCode::Success);

        // QueryRows(3): the served importance cells should be 5,3,1 (DESC).
        let qr = dispatch(&state, &sid, &[0x15, 0, 9, 0, 0, 3, 0]).await;
        let rc = u16::from_le_bytes([qr[7], qr[8]]);
        assert_eq!(rc, 3);
        // Each row is flag(1) + Integer32(4): off starts at 9.
        let vals: Vec<i32> = (0..3)
            .map(|i| {
                let base = 9 + i * 5;
                i32::from_le_bytes([qr[base + 1], qr[base + 2], qr[base + 3], qr[base + 4]])
            })
            .collect();
        assert_eq!(vals, vec![5, 3, 1]);
    }

    /// The synthetic Calendar folder row carries the `__calendar__` role so
    /// `mailbox_to_cells` renders `PR_CONTAINER_CLASS=IPF.Appointment` and
    /// `folder_kind_for_role` resolves to `FolderKind::Calendar` (closing
    /// audit gap S2c: the calendar folder was previously invisible to
    /// Outlook's hierarchy-table walk).
    #[test]
    fn synth_calendar_folder_row_has_ipf_appointment_class() {
        let row = synth_folder_row(
            store::CALENDAR_BACKEND_ID,
            "Calendar",
            store::CALENDAR_BACKEND_ID,
        );
        let src = row.source.expect("synth row carries a JmapMailbox source");
        let mbx = src
            .downcast_ref::<crate::jmap::JmapMailbox>()
            .expect("source is a JmapMailbox");
        assert_eq!(mbx.role.as_deref(), Some(store::CALENDAR_BACKEND_ID));
        assert_eq!(mbx.name.as_deref(), Some("Calendar"));
        let cs = [crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING,
            store::PR_CONTAINER_CLASS,
        )];
        let cells = store::mailbox_to_cells(mbx, &cs);
        use crate::mapi::data::PropertyValue;
        match &cells[0] {
            PropertyValue::String(s) => assert_eq!(s, "IPF.Appointment"),
            other => panic!("expected String cell, got {:?}", other),
        }
        assert_eq!(
            store::folder_kind_for_role(mbx.role.as_deref()),
            FolderKind::Calendar
        );
        assert_eq!(
            store::folder_kind_for_backend_id(store::CALENDAR_BACKEND_ID),
            Some(FolderKind::Calendar)
        );
    }

    /// The synthetic Contacts folder row renders `PR_CONTAINER_CLASS=IPF.Contact`.
    #[test]
    fn synth_contacts_folder_row_has_ipf_contact_class() {
        let row = synth_folder_row(
            store::CONTACTS_BACKEND_ID,
            "Contacts",
            store::CONTACTS_BACKEND_ID,
        );
        let mbx = row
            .source
            .as_ref()
            .and_then(|s| s.downcast_ref::<crate::jmap::JmapMailbox>())
            .expect("source is a JmapMailbox");
        let cs = [crate::mapi::data::PropertyTag::new(
            crate::mapi::data::PropertyType::PTYP_STRING,
            store::PR_CONTAINER_CLASS,
        )];
        let cells = store::mailbox_to_cells(mbx, &cs);
        use crate::mapi::data::PropertyValue;
        match &cells[0] {
            PropertyValue::String(s) => assert_eq!(s, "IPF.Contact"),
            other => panic!("expected String cell, got {:?}", other),
        }
        assert_eq!(
            store::folder_kind_for_role(mbx.role.as_deref()),
            FolderKind::Contacts
        );
        assert_eq!(
            store::folder_kind_for_backend_id(store::CONTACTS_BACKEND_ID),
            Some(FolderKind::Contacts)
        );
    }

    /// `parse_calendar_multistatus` turns a CalDAV `<C:calendar-data>`
    /// multistatus body into `TableRow`s carrying a cached `CalendarItem`,
    /// keyed off the iCalendar UID (via `parse_ics_event`).
    #[test]
    fn parse_calendar_multistatus_yields_calendar_item_rows() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:mtg-123@example\r\nSUMMARY:Standup\r\nDTSTART:20260101T090000Z\r\nDTEND:20260101T093000Z\r\nLOCATION:Room 1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let xml = format!(
            "<?xml version=\"1.0\"?>\n<D:multistatus xmlns:D=\"DAV:\" xmlns:C=\"urn:ietf:params:xml:ns:caldav\">\
             <D:response><D:href>/dav/cal/u/event.ics</D:href>\
             <D:propstat><D:prop><C:calendar-data>{}</C:calendar-data></D:prop></D:propstat>\
             </D:response></D:multistatus>",
            ics
        );
        let rows = parse_calendar_multistatus(&xml);
        assert_eq!(rows.len(), 1);
        let src = rows[0].source.as_ref().expect("row carries CalendarItem");
        let item = src
            .downcast_ref::<crate::calendar::CalendarItem>()
            .expect("source is a CalendarItem");
        assert_eq!(item.uid, "mtg-123@example");
        assert_eq!(item.subject, "Standup");
        // row_id is the FNV-1a of the UID - stable across sessions.
        assert_eq!(
            rows[0].row_id,
            store::message_id_from_jmap("mtg-123@example")
        );
    }

    /// An empty / malformed multistatus yields zero rows (fail-closed, no panic).
    #[test]
    fn parse_calendar_multistatus_empty_yields_no_rows() {
        assert!(parse_calendar_multistatus("").is_empty());
        assert!(parse_calendar_multistatus("<?xml version=\"1.0\"?><x/>").is_empty());
    }

    // ── FXICS upload apply → JMAP write-back bridge (audit gap #2) ───────
    //
    // The pure decoders + the message-bag patch builder are exercised
    // without a live server; the tokenize-only apply path (jmap = None) is
    // used to verify the walk accepts well-formed IncrSyncChg/Del/Read
    // streams and fails closed (`Err`) on a structurally malformed stream.

    /// Build an FXICS message-id cell (PR_MID = 0x6748, PtypInteger64 = 0x0014)
    /// for the IcsStreamBuilder: the 4-byte tag LE then the 8-byte LE value.
    fn pushfx_mid(b: &mut crate::mapi::fxics::IcsStreamBuilder, mid: u64) {
        let tag = ((store::PR_MID as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_INTEGER64.to_u16() as u32);
        b.push_property(tag, &mid.to_le_bytes());
    }

    #[test]
    fn fx_decode_raw_mid_reads_8_bytes_le() {
        let mid = store::message_id_from_jmap("M-1");
        assert_eq!(fx_decode_raw_mid(&mid.to_le_bytes()), Some(mid));
        // truncated payloads do not panic — fail-closed to None.
        assert_eq!(fx_decode_raw_mid(&[0u8; 7]), None);
        assert_eq!(fx_decode_raw_mid(&[]), None);
    }

    #[test]
    fn fx_decode_mid_cell_requires_pr_mid_integer64_tag() {
        let mid = store::message_id_from_jmap("M-2");
        let tag = ((store::PR_MID as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_INTEGER64.to_u16() as u32);
        assert_eq!(fx_decode_mid_cell(tag, &mid.to_le_bytes()), Some(mid));
        assert!(fx_is_mid_cell(tag));
        // A wrong property type (PtypInteger32) is not a message-id cell.
        let wrong_type = ((store::PR_MID as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_INTEGER32.to_u16() as u32);
        assert!(!fx_is_mid_cell(wrong_type));
        // A wrong property id is not a message-id cell.
        let wrong_id = ((store::PR_SUBJECT as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_INTEGER64.to_u16() as u32);
        assert!(!fx_is_mid_cell(wrong_id));
    }

    #[test]
    fn fx_decode_bool_and_i32_handle_pads_and_truncation() {
        // PtypBoolean 2-byte form: nonzero first byte is true.
        assert_eq!(fx_decode_bool(&[0x01, 0x00]), Some(true));
        assert_eq!(fx_decode_bool(&[0x00, 0x00]), Some(false));
        assert_eq!(fx_decode_bool(&[0x00, 0x01]), Some(true));
        assert_eq!(fx_decode_bool(&[0u8; 1]), None);
        // PtypInteger32 4-byte LE.
        assert_eq!(fx_decode_i32(&[0x40, 0x00, 0x00, 0x00]), Some(0x40));
        assert_eq!(fx_decode_i32(&[0u8; 3]), None);
    }

    #[test]
    fn fx_decode_string_handles_utf16le_and_string8() {
        use crate::mapi::data::PropertyType as T;
        // "Hi" as UTF-16LE = 0x48 0x00 0x69 0x00.
        assert_eq!(
            fx_decode_string(T::PTYP_STRING, &[0x48, 0x00, 0x69, 0x00]),
            Some("Hi".to_string())
        );
        assert_eq!(
            fx_decode_string(T::PTYP_STRING8, b"Hello"),
            Some("Hello".to_string())
        );
        // Non-string types are not decoded here.
        assert_eq!(fx_decode_string(T::PTYP_INTEGER32, &[0u8; 4]), None);
    }

    #[test]
    fn fx_message_bag_mid_and_patch_round_trip() {
        use crate::mapi::data::PropertyType as T;
        let mid = store::message_id_from_jmap("M-3");
        let mut bag = FxMessageBag::default();
        // PR_MID cell.
        let mid_tag = ((store::PR_MID as u32) << 16) | (T::PTYP_INTEGER64.to_u16() as u32);
        bag.push(mid_tag, mid.to_le_bytes().to_vec());
        // PR_SUBJECT cell (PtypString UTF-16LE, no NUL in the FXICS raw form).
        let subj_tag = ((store::PR_SUBJECT as u32) << 16) | (T::PTYP_STRING.to_u16() as u32);
        let subj_utf16: Vec<u16> = "Test".encode_utf16().collect();
        let mut subj_bytes = Vec::new();
        for u in subj_utf16 {
            subj_bytes.extend_from_slice(&u.to_le_bytes());
        }
        // FXICS string carries a 4-byte length prefix — the tokenizer strips
        // it before handing the raw bytes to the bag, so prepend it here to
        // mirror the builder; the bag stores the *payload* (no prefix) so we
        // push the payload directly.
        bag.push(subj_tag, subj_bytes.clone());
        // PR_MESSAGE_FLAGS = 0x40 (read) — PtypInteger32.
        let flags_tag =
            ((store::PR_MESSAGE_FLAGS as u32) << 16) | (T::PTYP_INTEGER32.to_u16() as u32);
        bag.push(flags_tag, 0x40u32.to_le_bytes().to_vec());

        assert_eq!(bag.mid(), Some(mid));
        let patch = bag.to_jmap_patch().expect("patch present");
        let obj = patch.as_object().expect("object");
        assert_eq!(obj.get("subject").and_then(|v| v.as_str()), Some("Test"));
        // read flag (MSGFLAG_READ 0x40) -> keywords/$seen = true.
        assert_eq!(obj.get("keywords/$seen"), Some(&serde_json::json!(true)));
    }

    #[test]
    fn fx_message_bag_no_translatable_props_yields_no_patch() {
        // A bag with only a PR_MID and an unrecognised property: the patch is
        // None but the mid still resolves.
        let mid = store::message_id_from_jmap("M-4");
        let mut bag = FxMessageBag::default();
        let mid_tag = ((store::PR_MID as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_INTEGER64.to_u16() as u32);
        bag.push(mid_tag, mid.to_le_bytes().to_vec());
        // An unrelated property id the bag does not translate.
        bag.push(0x0000ABCD, vec![0u8; 4]);
        assert_eq!(bag.mid(), Some(mid));
        assert!(bag.to_jmap_patch().is_none());
    }

    #[tokio::test]
    async fn apply_fasttransfer_upload_tokenize_only_accepts_inc_sync_del_stream() {
        // Build an IncrSyncDel upload stream carrying two mids. With no JMAP
        // backend the apply tokenises + reports success (the no-backend →
        // tokenize-only contract); the walk just records the events.
        let mid_a = store::message_id_from_jmap("M-5");
        let mid_b = store::message_id_from_jmap("M-6");
        let mut b = crate::mapi::fxics::IcsStreamBuilder::new();
        b.push_marker(crate::mapi::fxics::Marker::IncrSyncDel);
        pushfx_mid(&mut b, mid_a);
        pushfx_mid(&mut b, mid_b);
        let buf = b.finish(); // appends IncrSyncEnd
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &buf, 0, "inbox").await;
        assert!(res.is_ok(), "well-formed Del stream: {res:?}");
    }

    #[tokio::test]
    async fn apply_fasttransfer_upload_tokenize_only_accepts_read_stream() {
        // IncrSyncRead: two (mid, read-flag) pairs.
        let mid_a = store::message_id_from_jmap("M-7");
        let mid_b = store::message_id_from_jmap("M-8");
        let bool_tag = ((store::PR_READ as u32) << 16)
            | (crate::mapi::data::PropertyType::PTYP_BOOLEAN.to_u16() as u32);
        let mut b = crate::mapi::fxics::IcsStreamBuilder::new();
        b.push_marker(crate::mapi::fxics::Marker::IncrSyncRead);
        pushfx_mid(&mut b, mid_a);
        b.push_property(bool_tag, &[0x01, 0x00]); // read = true
        pushfx_mid(&mut b, mid_b);
        b.push_property(bool_tag, &[0x00, 0x00]); // read = false
        let buf = b.finish();
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &buf, 0, "inbox").await;
        assert!(res.is_ok(), "well-formed Read stream: {res:?}");
    }

    #[tokio::test]
    async fn apply_fasttransfer_upload_fail_closed_on_unbalanced_marker() {
        // A stream that opens StartTopFld then ends with IncrSyncEnd (so an
        // EndFolder is missing) leaves a start marker open — assert_complete
        // rejects it with a DecodeError (the fail-closed contract).
        let mut buf = Vec::new();
        buf.extend_from_slice(
            &crate::mapi::fxics::Marker::StartTopFld
                .value()
                .to_le_bytes(),
        );
        buf.extend_from_slice(
            &crate::mapi::fxics::Marker::IncrSyncEnd
                .value()
                .to_le_bytes(),
        );
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &buf, 0, "inbox").await;
        assert!(res.is_err(), "unbalanced stream must fail closed: {res:?}");
    }

    #[tokio::test]
    async fn apply_fasttransfer_upload_empty_buffer_is_ok() {
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &[], 0, "inbox").await;
        assert!(res.is_ok());
    }

    #[tokio::test]
    async fn apply_fasttransfer_upload_tokenize_only_accepts_chg_stream() {
        // IncrSyncChg + IncrSyncMessage bags carrying PR_MID + editable props.
        // With a PR_MESSAGE_FLAGS read bit the bag would build a $seen patch on
        // a wired backend; here we only assert the tokenize-only walk succeeds.
        use crate::mapi::data::PropertyType as T;
        let mid = store::message_id_from_jmap("M-9");
        let mid_tag = ((store::PR_MID as u32) << 16) | (T::PTYP_INTEGER64.to_u16() as u32);
        let flags_tag =
            ((store::PR_MESSAGE_FLAGS as u32) << 16) | (T::PTYP_INTEGER32.to_u16() as u32);
        let mut b = crate::mapi::fxics::IcsStreamBuilder::new();
        b.push_marker(crate::mapi::fxics::Marker::IncrSyncChg);
        b.push_marker(crate::mapi::fxics::Marker::IncrSyncMessage);
        // IcsStreamBuilder.push_property takes value bytes (no length for
        // fixed types); the 8-byte Integer64 mid.
        b.push_property(mid_tag, &mid.to_le_bytes());
        b.push_property(flags_tag, &0x40u32.to_le_bytes()); // read bit set
        b.push_marker(crate::mapi::fxics::Marker::EndMessage);
        let buf = b.finish();
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &buf, 0, "inbox").await;
        assert!(res.is_ok(), "well-formed Chg stream: {res:?}");
    }

    #[test]
    fn fx_message_bag_move_dest_mid_detects_cross_folder_move() {
        use crate::mapi::data::PropertyType as T;
        let parent_mid = store::folder_id_from_backend("inbox");
        let other_mid = store::folder_id_from_backend("archive");
        // A bag carrying PR_FOLDER_ID == other_mid signals a move out of parent.
        let mut bag = FxMessageBag::default();
        let folder_tag = ((store::PR_FOLDER_ID as u32) << 16) | (T::PTYP_INTEGER64.to_u16() as u32);
        bag.push(folder_tag, other_mid.to_le_bytes().to_vec());
        assert_eq!(bag.move_dest_mid(parent_mid), Some(other_mid));
        // A bag whose PR_FOLDER_ID matches the parent is NOT a cross-folder move.
        let mut bag2 = FxMessageBag::default();
        bag2.push(folder_tag, parent_mid.to_le_bytes().to_vec());
        assert_eq!(bag2.move_dest_mid(parent_mid), None);
    }

    /// The batched read-state update object keys each accepted id with the
    /// RFC 8621 §4.5 `keywords/$seen` patch — NO leading slash (RFC 8620
    /// PatchObject keys are implicit). Unresolved mids are skipped, not
    /// failed; the (applied, skipped) counts reflect the split.
    #[test]
    fn fx_build_read_update_keys_and_batching() {
        use std::collections::HashMap;
        let mid_a = store::message_id_from_jmap("E-1");
        let mid_b = store::message_id_from_jmap("E-2");
        let mut mid_to_jmap: HashMap<u64, (String, Vec<String>)> = HashMap::new();
        mid_to_jmap.insert(mid_a, ("E-1".to_string(), vec!["inbox".to_string()]));
        mid_to_jmap.insert(mid_b, ("E-2".to_string(), vec!["inbox".to_string()]));
        // mid_c is NOT in the map -> skipped.
        let mid_c = store::message_id_from_jmap("E-3");
        let pairs = vec![(mid_a, true), (mid_b, false), (mid_c, true)];
        let (update, applied, skipped) = fx_build_read_update(&mid_to_jmap, &pairs);
        assert_eq!(applied, 2);
        assert_eq!(skipped, 1);
        let obj = update.as_object().expect("object");
        // Two ids keyed by JMAP id, no leading slash on the patch key.
        assert!(obj.contains_key("E-1"));
        assert!(obj.contains_key("E-2"));
        let patch_a = obj
            .get("E-1")
            .and_then(|v| v.as_object())
            .expect("E-1 patch");
        assert_eq!(
            patch_a.get("keywords/$seen"),
            Some(&serde_json::json!(true))
        );
        let patch_b = obj
            .get("E-2")
            .and_then(|v| v.as_object())
            .expect("E-2 patch");
        assert_eq!(
            patch_b.get("keywords/$seen"),
            Some(&serde_json::Value::Null)
        );
        // No leading-slash form leaked.
        assert!(patch_a.get("/keywords/$seen").is_none());
        assert!(patch_b.get("/keywords/$seen").is_none());
    }

    /// The move update patch emits RFC 8620 PatchObject keys with NO leading
    /// slash: `mailboxIds/<target>: true` and `mailboxIds/<current>: null`,
    /// matching the canonical `build_move_update_patch` in `jmap.rs`.
    #[test]
    fn fx_build_move_update_no_leading_slash_and_target_set() {
        let mids = vec!["inbox".to_string(), "drafts".to_string()];
        let patch = fx_build_move_update(&mids, "archive");
        assert_eq!(
            patch.get("mailboxIds/archive"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            patch.get("mailboxIds/inbox"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(
            patch.get("mailboxIds/drafts"),
            Some(&serde_json::Value::Null)
        );
        // No leading-slash variant present (the regression the comment caught).
        assert!(patch.get("/mailboxIds/archive").is_none());
        assert!(patch.get("/mailboxIds/inbox").is_none());
        assert!(patch.get("/mailboxIds/drafts").is_none());
        // The target is not double-listed (no null for the destination).
        // (fx_build_move_update only nulls `old != dest`, so target is set once
        // via the trailing insert.)
        assert_eq!(patch.len(), 3);
    }

    /// Cross-folder destination resolution uses the FOLDER-mid map keyed by
    /// `folder_id_from_backend(jmap_mailbox_id)`, NOT the message-id map. A
    /// folder mid looked up in `mid_to_jmap` would never resolve (different
    /// hash family); this test documents that the move path must consult
    /// `folder_mid_to_mailbox` to recover the real JMAP mailbox id.
    #[test]
    fn folder_mid_to_mailbox_resolves_dest_not_message_map() {
        use std::collections::HashMap;
        let inbox_mailbox_id = "M-inbox".to_string();
        let archive_mailbox_id = "M-archive".to_string();
        let email_jid = "E-1".to_string();
        // folder mid of the archive mailbox == folder_id_from_backend(id).
        let archive_folder_mid = store::folder_id_from_backend(&archive_mailbox_id);
        let message_mid = store::message_id_from_jmap(&email_jid);
        // The two hash families are DISTINCT (this is the crux of the fix).
        assert_ne!(archive_folder_mid, message_mid);
        let mut mid_to_jmap: HashMap<u64, (String, Vec<String>)> = HashMap::new();
        mid_to_jmap.insert(
            message_mid,
            (email_jid.clone(), vec![inbox_mailbox_id.clone()]),
        );
        let mut folder_mid_to_mailbox: HashMap<u64, String> = HashMap::new();
        folder_mid_to_mailbox.insert(archive_folder_mid, archive_mailbox_id.clone());
        // Looking the archive folder mid up in mid_to_jmap MUST fail (the old
        // bug); looking it up in folder_mid_to_mailbox MUST resolve.
        assert!(!mid_to_jmap.contains_key(&archive_folder_mid));
        assert_eq!(
            folder_mid_to_mailbox.get(&archive_folder_mid),
            Some(&archive_mailbox_id)
        );
        // And the move patch against the resolved id has the no-leading-slash
        // form (RFC 8620 PatchObject).
        let mids = vec![inbox_mailbox_id];
        let patch = fx_build_move_update(&mids, &archive_mailbox_id);
        assert_eq!(
            patch.get("mailboxIds/M-archive"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            patch.get("mailboxIds/M-inbox"),
            Some(&serde_json::Value::Null)
        );
        assert!(patch.get("/mailboxIds/M-archive").is_none());
        assert!(patch.get("/mailboxIds/M-inbox").is_none());
    }

    /// Regression: an interleaved (non-PR_MID, non-boolean) cell in an
    /// IncrSyncRead span no longer discards the pending mid before the
    /// read-flag cell arrives — it is preserved until a real read value
    /// decodes. The walk loop asserts this end-to-end against a wired-None
    /// (tokenize-only) apply; here we assert the pure invariant the loop now
    /// honours: a `bool`-decodable cell with a pending mid pairs; a
    /// non-decodable cell leaves it.
    #[tokio::test]
    async fn apply_read_span_preserves_pending_mid_across_unrelated_cell() {
        // Stream: IncrSyncRead; PR_MID(E-9); an unrelated 1-byte cell (NOT a
        // boolean payload); PR_MESSAGE_FLAGS=0x40 (read). The mid E-9 must
        // survive the unrelated cell so the read pair is captured.
        use crate::mapi::data::PropertyType as T;
        let mid = store::message_id_from_jmap("E-9");
        let flags_tag =
            ((store::PR_MESSAGE_FLAGS as u32) << 16) | (T::PTYP_INTEGER32.to_u16() as u32);
        // An unrelated Integer32 cell with id 0xABCD (not PR_MID, not a
        // boolean) — under the old tuple-take bug this would clear the mid.
        let noise_tag = (0xABCDu32 << 16) | (T::PTYP_INTEGER32.to_u16() as u32);
        let mut b = crate::mapi::fxics::IcsStreamBuilder::new();
        b.push_marker(crate::mapi::fxics::Marker::IncrSyncRead);
        pushfx_mid(&mut b, mid);
        b.push_property(noise_tag, &[0u8; 4]); // interleaved non-bool cell
        b.push_property(flags_tag, &0x40u32.to_le_bytes()); // read flag
        let buf = b.finish();
        let res = apply_fasttransfer_upload(None, None, "u@example.com", &buf, 0, "inbox").await;
        assert!(
            res.is_ok(),
            "read span with interleaved cell must still parse: {res:?}"
        );
    }
}
