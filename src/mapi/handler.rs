// src/mapi/handler.rs
//
// The MAPI/HTTP request orchestrator: takes a parsed `MapiRequest` from
// `transport.rs`, dispatches by X-RequestType, and produces the
// `MapiResponse` the axum route renders.
//
// Phase 0 dispatch:
//   * Connect    — parse the ROP buffer, dispatch the leading ROP (which
//                  must be RopLogon for a fresh connection), authenticate via
//                  Basic auth, allocate a session, encode the RopLogonSuccess.
//   * Execute    — look up the session (by the X-ClientInfo / X-Connection
//                  cookie path), parse the requested RopId, and delegate to
//                  the matching ROP handler. Phase 0 implements a stable
//                  dispatch table for the Phase-0 ROP set; unknown ROPs and
//                  the address-book-only ROP set return a typed error
//                  envelope (code 5 / InvalidRequestType at the transport
//                  layer; for an in-session Execute the ROP-level envelope
//                  returns InvalidParameter).
//   * Disconnect — drop the named session and return code 0.
//   * NotificationWait / PING — Phase 0 returns a deterministic
//                  success-with-empty-body response (the long-poll behaviour
//                  lands in Phase 1).
//
// All decode paths fail closed: an unrecoverable buffer under-run or an
// unexpected RopId on a `Connect` returns a transport-layer
// `ResponseCode::InvalidRequestBody` (12).

use crate::auth::AuthVerifier;
use crate::config::Config;
use crate::mapi::logon::{LogonOutcome, logon_basic};
use crate::mapi::rops::{
    Buf, DecodeError, RopCommitStreamRequest, RopCommitStreamResponse, RopCopyToRequest,
    RopCopyToSuccess, RopCreateAttachmentRequest, RopCreateMessageRequest,
    RopCreateMessageSuccess, RopDeleteAttachmentRequest, RopDeleteAttachmentResponse,
    RopDeleteMessagesRequest, RopDeleteMessagesResponse, RopDeletePropertiesRequest,
    RopErrorCode, RopErrorResponse, RopGetAttachmentTableRequest, RopGetAttachmentTableSuccess,
    RopGetPropertiesAllRequest, RopGetPropertiesSpecificRequest, RopGetStatusRequest,
    RopGetStreamSizeRequest, RopGetStreamSizeSuccess, RopGetValidAttachmentsRequest,
    RopGetValidAttachmentsSuccess, RopHeader4, RopId, RopLogonRequest, RopLogonSuccess,
    RopMoveCopyMessagesRequest, RopMoveCopyMessagesResponse, RopOpenAttachmentRequest,
    RopOpenAttachmentSuccess, RopOpenStreamRequest, RopOpenStreamSuccess, RopOpenTableRequest,
    RopPropertyWriteSuccess, RopQueryRowsRequest, RopReadStreamRequest, RopReadStreamSuccess,
    RopReleaseRequest, RopSaveChangesAttachmentRequest, RopSaveChangesAttachmentResponse,
    RopSaveChangesMessageRequest, RopSaveChangesMessageSuccess, RopSeekStreamRequest,
    RopSeekStreamSuccess, RopSetColumnsRequest, RopSetMessageReadFlagRequest,
    RopSetPropertiesRequest, RopSetStreamSizeRequest, RopSetStreamSizeResponse,
    RopSubmitMessageRequest, RopSubmitMessageResponse, RopTransportSendFailure,
    RopTransportSendRequest, RopTransportSendSuccess, RopWriteStreamRequest, RopWriteStreamSuccess,
};
use crate::mapi::session::{FolderKind, Handle, SessionManager};
use crate::mapi::store;
use crate::mapi::transport::{MapiRequest, MapiRequestType, MapiResponse, ResponseCode, RpcKind};

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
    /// `NotificationWait` long-poll sees the change — closing the EWS-only
    /// notification gap where a MAPI-triggered property write raised no event
    /// and the client aggressively re-polled (qodo #9, cubic #30, audit §2e).
    /// `None` in unit-test fixtures keeps them free of a live manager.
    pub subscription_manager:
        Option<std::sync::Arc<crate::notifications::SubscriptionManager>>,
    /// Optional gateway attachment store (the same `AttachmentManager` EWS /
    /// EAS already use). When present, the MAPI attachment write path
    /// (`RopSaveChangesAttachment` after `Blob/upload`, `RopDeleteAttachment`)
    /// persists the (email_id → blob_id/name/content_type/attach_num) record so
    /// `RopGetAttachmentTable` / `RopOpenAttachment` / `RopGetValidAttachments`
    /// round-trip a MAPI-composed attachment against Stalwart. `None` in
    /// unit-test fixtures keeps the write path free of an SQLite handle.
    pub attachment_manager: Option<std::sync::Arc<crate::attachment::AttachmentManager>>,
}

impl MapiState {
    pub fn new(cfg: Config, auth: std::sync::Arc<AuthVerifier>) -> Self {
        Self {
            cfg,
            auth,
            sessions: SessionManager::new(),
            subscription_manager: None,
            attachment_manager: None,
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
        }
    }

    /// Wire the gateway attachment store onto the MAPI state. Called by the
    /// production `AppState` builder so `RopSaveChangesAttachment` /
    /// `RopDeleteAttachment` can persist MAPI-composed attachments against
    /// the same SQLite-backed manager EWS/EAS share.
    pub fn with_attachment_manager(mut self, mgr: std::sync::Arc<crate::attachment::AttachmentManager>) -> Self {
        self.attachment_manager = Some(mgr);
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
            // Long-poll: Phase 0 returns empty success so the client backoffs.
            MapiResponse::success(req.request_id, "NotificationWait", None, Vec::new())
        }
        RpcKind::Mailbox(MapiRequestType::Ping) => {
            MapiResponse::success(req.request_id, "PING", None, Vec::new())
        }
        RpcKind::AddressBook => {
            // Address-book endpoint ROPs are not dispatched in Phase 0.
            MapiResponse::error(ResponseCode::InvalidRequestType, req.request_id)
        }
    }
}

/// `Connect` RPC: the leading ROP must be `RopLogon`. On success we allocate
/// a session and emit the success envelope. We do NOT carry a transport-level
/// session cookie in Phase 0 — Outlook will re-Connect if the server returns
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
            // Per MS-OXCMAPIHTTP §3.2.5.1 / §4.1, the server MUST return the
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

/// `Execute` RPC: a buffer of one or more ROPs (MS-OXCROPS §3.2.5), each with
/// its own RopId + (LogonId) + handle indices. We decode them in order,
/// dispatch to a per-ROP handler that may bridge to the Stalwart backend,
/// and concatenate the per-ROP response bytes into a single Execute body.
///
/// Per MS-OXCMAPIHTTP §3.2.5.2 the Session Context is identified by the
/// `Cookie: MapiContext=<opaque>` header the client echoes after Connect.
/// We honour the cookie first and fall back to the (optional) X-ClientInfo
/// extension UUID emitted at RopLogon time, so the in-process unit tests
/// that drive this handler directly still resolve the session.
async fn handle_execute(req: MapiRequest, state: &MapiState) -> MapiResponse {
    let session_id = crate::mapi::transport::cookie_value(&req.cookies, "MapiContext")
        .and_then(|v| uuid::Uuid::parse_str(v).ok())
        .or_else(|| req.client_info.as_deref().and_then(parse_client_info_uuid));
    let Some(session_id) = session_id else {
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

    // One cheap JmapClient per Execute — the session cache inside it caches
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
            // stop the chain — the client will re-issue the unacked ROPs.
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

/// Discriminant for the shape of a handle resolved GetProperties* — keeps
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
            // §2.2.15.3.1: LogonId + InputHandleIndex
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let _ = RopReleaseRequest::decode(cur)?;
            sessions.with_session_mut(session_id, |s| s.free_handle(input_handle_index));
            crate::mapi::rops::RopReleaseResponse {
                input_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        RopId::ROP_OPEN_FOLDER => {
            // §2.2.4.1.1: 4-byte header then FolderId(8) + OpenModeFlags(1).
            // The dispatcher consumed the leading RopId byte before entering
            // this branch, so use `decode_after_ropid` to read only the
            // remaining LogonId·Input·Output bytes (RopHeader4::decode would
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
                let rows = mailboxes
                    .map(|ml| ml.mailboxes)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|mbx| {
                        let bid = mbx.id.clone().unwrap_or_default();
                        let row_id = store::folder_id_from_backend(&bid);
                        let cells: Vec<crate::mapi::data::PropertyValue> = Vec::new();
                        // Synthesize Calendar/Contacts rows (JMAP has no
                        // mailbox for those — CalDAV/CardDAV own them) so
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
                let total = rows.len() as u64;
                (rows, total, FolderKind::Mail)
            } else {
                // Contents table: enumerate the messages in the parent folder.
                let rows = if let Some(jc) = jmap {
                    if let Some(pw) = password {
                        fetch_contents_rows(jc, username, pw, &parent_backend, parent_kind).await
                    } else {
                        Vec::new()
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
            // §2.2.5.1.1: LogonId + InputHandleIndex + SetColumnFlags(1)
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
            // §2.2.5.4.1: LogonId + InputHandleIndex + QueryRowsFlags(1)
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
                        ..
                    }) = s.handle_mut(input_handle_index)
                    else {
                        return (Vec::new(), 0u16, 0u8);
                    };
                    let cs = column_set.clone();
                    let pk = *kind;
                    let mailbox_id = parent_backend_id.clone();
                    let want =
                        usize::from(req.row_count).min(rows.len() - (*cursor).min(rows.len()));
                    let mut buf = Vec::new();
                    let served = u16::try_from(want).unwrap_or(0);
                    for r in rows.iter_mut().skip(*cursor).take(want) {
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
                            }
                        }
                        // Emit a StandardPropertyRow (flag=0): one flag byte
                        // + the per-column PropertyValue bytes (no tag prefix
                        // — the column order echoes the SetColumns request).
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
            let (handle_shape, kind, backend_id, mailbox_id) = sessions
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
                    ),
                    Handle::Folder { backend_id, kind } => (
                        HandleShape::Folder,
                        *kind,
                        backend_id.clone(),
                        String::new(),
                    ),
                    // An Attachment handle resolves GetPropertiesSpecific
                    // against the live JMAP email's indexed attachment. The
                    // `_attach_num` is captured only to document the field's
                    // role here — the real value is re-read in a second
                    // snapshot below (the 4-tuple has no slot for it without
                    // re-plumbing the whole message/folder cells path).
                    Handle::Attachment {
                        email_id,
                        kind,
                        attach_num: _attach_num,
                        ..
                    } => (HandleShape::Attachment, *kind, email_id.clone(), String::new()),
                    _ => (
                        HandleShape::Neither,
                        FolderKind::Root,
                        String::new(),
                        String::new(),
                    ),
                })
                .unwrap_or((
                    HandleShape::Neither,
                    FolderKind::Root,
                    String::new(),
                    String::new(),
                ));
            // For an Attachment handle, `attach_num` is needed to pick the
            // indexed attachment out of the email's `attachments[]`; resolve it
            // in a second read-side snapshot (cheap — one String + small table
            // clone).
            let attach_num = if handle_shape == HandleShape::Attachment {
                sessions
                    .with_handle(session_id, input_handle_index, |h| match h {
                        Handle::Attachment { attach_num, .. } => *attach_num,
                        _ => 0,
                    })
                    .unwrap_or(0)
            } else {
                0
            };
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
                // Mail attachment handle -> Email/get -> resolve the indexed
                // attachment (by PR_ATTACH_NUM) -> attachment_to_cells
                // (PR_ATTACH_LONG_FILENAME/PR_ATTACH_MIME_TAG/PR_ATTACH_SIZE/
                // PR_ATTACH_METHOD/PR_ATTACH_NUM/...). A MAPI-created
                // attachment whose bytes were staged and uploaded, or one that
                // the gateway's AttachmentManager owns by email id, is also
                // surfaced here so GetProperties is consistent with the table.
                (HandleShape::Attachment, Some(jc), Some(pw))
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
                            Ok(Some(e)) => match store::email_attachment_by_num(&e, attach_num) {
                                Some(att) => {
                                    store::attachment_to_cells(att, attach_num, &req.property_tags)
                                }
                                None => store::typed_null_cells(&req.property_tags),
                            },
                            _ => store::typed_null_cells(&req.property_tags),
                        }
                    }
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
            // §2.2.6.17.1: RopId · LogonId · InputHandleIndex ·
            // OutputHandleIndex · TableFlags(1) — a 4-byte RopHeader4 body
            // followed by TableFlags. The input handle is the Message whose
            // attachments we enumerate; the output handle is the new Table.
            let h4 = RopHeader4::decode_after_ropid(cur, rop_id)?;
            let req = RopGetAttachmentTableRequest::decode_body(
                cur,
                h4.input_handle_index,
                h4.output_handle_index,
            )?;
            // Resolve the owning message (mail only — calendar/contact
            // attachments are not enumerated through MAPI in this phase).
            let (email_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message {
                        backend_id,
                        kind,
                        ..
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
            // real `attachments[]`; on any failure install an empty table so
            // the client sees "no attachments" rather than NotFound.
            let rows = if let (Some(jc), Some(pw)) = (jmap, password) {
                let account_id = jc
                    .get_account_id(username, pw)
                    .await
                    .ok()
                    .unwrap_or_default();
                if account_id.is_empty() {
                    Vec::new()
                } else {
                    match jc.get_email(&account_id, &email_id, username, pw).await {
                        Ok(Some(e)) => store::email_attach_nums(&e)
                            .into_iter()
                            .map(|num| {
                                let att =
                                    store::email_attachment_by_num(&e, num).cloned();
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
                        _ => Vec::new(),
                    }
                }
            } else {
                Vec::new()
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
                    },
                );
            });
            // Spec §2.2.6.17.2: the response is the success envelope only
            // (RopId · OutputHandleIndex · ReturnValue). The row count is
            // delivered via the subsequent RopQueryRows against the table, not
            // in this envelope, so we do not append one here.
            RopGetAttachmentTableSuccess {
                output_handle_index: req.output_handle_index,
                return_value: RopErrorCode::Success,
            }
            .encode(out);
        }
        RopId::ROP_OPEN_ATTACHMENT => {
            // §2.2.6.12.1: RopId · LogonId · InputHandleIndex ·
            // OutputHandleIndex · OpenAttachmentFlags(1) · AttachmentID(4 LE).
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
            // success capture the JMAP blob id so a subsequent
            // `RopOpenStream(PR_ATTACH_DATA_BIN)` resolves the specific blob
            // (and the >1-attachment case that previously failed closed now
            // works through this handle).
            let (return_value, blob_id, name, content_type) = match (jmap, password) {
                (Some(jc), Some(pw)) => {
                    let account_id = jc
                        .get_account_id(username, pw)
                        .await
                        .ok()
                        .unwrap_or_default();
                    if account_id.is_empty() {
                        (RopErrorCode::NotFound, String::new(), String::new(), String::new())
                    } else {
                        match jc.get_email(&account_id, &email_id, username, pw).await {
                            Ok(Some(e)) => match store::email_attachment_by_num(&e, req.attachment_id) {
                                Some(att) => (
                                    RopErrorCode::Success,
                                    att.blob_id.clone().unwrap_or_default(),
                                    att.name.clone().unwrap_or_default(),
                                    att
                                        .content_type
                                        .clone()
                                        .unwrap_or_else(|| "application/octet-stream".to_string()),
                                ),
                                None => (
                                    RopErrorCode::NotFound,
                                    String::new(),
                                    String::new(),
                                    String::new(),
                                ),
                            },
                            Ok(None) => (
                                RopErrorCode::NotFound,
                                String::new(),
                                String::new(),
                                String::new(),
                            ),
                            Err(e) => {
                                tracing::warn!(error = %e, "JMAP get_email (OpenAttachment) failed");
                                (
                                    RopErrorCode::DiskError,
                                    String::new(),
                                    String::new(),
                                    String::new(),
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
            // §2.2.6.18.1: RopId · LogonId · InputHandleIndex.
            let _logon = cur.take_u8()?;
            let req = RopGetValidAttachmentsRequest::decode_after_ropid(cur)?;
            // The input handle MUST be a mail Message; enumerate its
            // `PR_ATTACH_NUM` ids from the live JMAP email.
            let (email_id, kind) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message { backend_id, kind, .. } => (backend_id.clone(), *kind),
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
                        _ => Vec::new(),
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
            // Per MS-OXCROPS §2.2.6.11.1 the post-RopId bytes are
            // LogonId · ResponseHandleIndex · InputHandleIndex · ReadFlags.
            // Consume all three header bytes here: the InputHandleIndex is
            // the Message handle, ResponseHandleIndex is what we echo back in
            // the response, ReadFlags is the body.
            let _logon = cur.take_u8()?;
            let response_handle_index = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSetMessageReadFlagRequest::decode(cur)?;
            // ReadFlags (MS-OXCMSG §2.2.3.11.1) is a BITMASK, not an
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
            // (MS-OXCROPS §2.2.6.11.2), NOT the InputHandleIndex used to
            // identify the message.
            RopErrorResponse {
                rop_id,
                output_handle_index: response_handle_index,
                return_value: outcome,
            }
            .encode(out);
        }
        // ---- Mail write path (audit §2a): compose / save / send / delete / move-
        // All six arms bridge to JMAP `Email/set` (create/update), `Email/destroy`,
        // `Email/set` mailboxIds patch (move) / `Email/set` copyFrom (copy), and
        // `EmailSubmission/set` (send). A missing JMAP backend, missing creds, or
        // an unbound handle yields a typed ROP-level error (NotFound /
        // AccessDenied / DiskError) so the client can react instead of a silent
        // Success-with-empty-state.
        RopId::ROP_CREATE_MESSAGE => {
            // §2.2.6.2.1: LogonId · InputHandleIndex · OutputHandleIndex ·
            // CodePageId(2) · FolderId(8) · AssociatedFlag(1). The dispatcher
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
            // §2.2.6.3.1: LogonId · ResponseHandleIndex · InputHandleIndex ·
            // SaveFlags(1).
            let _logon = cur.take_u8()?;
            // RopHeader's 3rd byte is unused here; the decoder's `_header_handle`
            // param accommodates the optional ignored handle, but the spec wire
            // after LogonId is ResponseHandleIndex · InputHandleIndex ·
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
            // The body write-back bridge (Blob/upload-backed Email/set
            // body-values) is not yet wired, so a dirty body stream cannot be
            // persisted here. Detect any dirty stream owned by this message
            // whose tag is a message body (PR_BODY / PR_BODY_HTML /
            // PR_RTF_COMPRESSED) and surface `NoSupport` so the client is not
            // told Success while the staged bytes are dropped (coderabbit,
            // matches the `is_dirty` doc on Handle::Stream). Non-body dirty
            // streams (e.g. future attachment writes) likewise fall back to a
            // bare draft create with no body patch.
            let has_dirty_body_stream = sessions
                .with_session_mut(session_id, |s| {
                    s.handles.values().any(|h| {
                        matches!(
                            h,
                            Handle::Stream {
                                source_handle_index,
                                property_tag,
                                is_dirty,
                                read_only,
                                ..
                            } if *is_dirty
                                && !*read_only
                                && *source_handle_index == req.input_handle_index
                                && store::is_body_stream_tag(property_tag)
                        )
                    })
                })
                .unwrap_or(false);
            let outcome: RopErrorCode;
            let saved_mid: u64;
            if has_dirty_body_stream {
                outcome = RopErrorCode::NoSupport;
                saved_mid = store::message_id_from_jmap(&backend_id);
                RopSaveChangesMessageSuccess {
                    response_handle_index: req.response_handle_index,
                    return_value: outcome,
                    input_handle_index: req.input_handle_index,
                    message_id: saved_mid,
                }
                .encode(out);
                return Ok(());
            }
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
                        match jc
                            .create_email(&account_id, email_obj, username, pw)
                            .await
                        {
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
            // §2.2.4.11.1: LogonId · InputHandleIndex · WantAsynchronous(1)
            // · NotifyNonRead(1) · MessageIdCount(2) · MessageIds[count×8].
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
            match (jmap, password, parent_kind == FolderKind::Mail && !parent_backend.is_empty()) {
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
                                            // §2.2.4.11.2: a value of 1 means
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
            // §2.2.4.6.1: LogonId · SourceHandleIndex · DestHandleIndex ·
            // MessageIdCount(2) · MessageIds[count×8] · WantAsynchronous(1)
            // · WantCopy(1).
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
                                        .copy_emails(&account_id, &jids, &dest_backend, username, pw)
                                        .await
                                    {
                                        Ok(n) => {
                                            outcome = RopErrorCode::Success;
                                            // PartialCompletion=1 unless the
                                            // server created a copy for every
                                            // requested id (found + processed).
                                            // MS-OXCROPS §2.2.4.6.4.
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
                                        .move_emails(&account_id, &jids, &dest_backend, username, pw)
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
            // §2.2.7.1.1: LogonId · InputHandleIndex · SubmitFlags(1).
            let _logon = cur.take_u8()?;
            let req = RopSubmitMessageRequest::decode_after_ropid(cur)?;
            // Resolve the message handle (must be a saved, non-new draft with a
            // real backend id) so we can drive EmailSubmission/set.
            let (backend_id, is_new) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message { backend_id, is_new, .. } => (backend_id.clone(), *is_new),
                    _ => (String::new(), false),
                })
                .unwrap_or((String::new(), false));
            let outcome: RopErrorCode = match (jmap, password, is_new, !backend_id.is_empty()) {
                (_, _, true, _) => RopErrorCode::InvalidParameter, // unsaved draft
                (_, _, _, false) => RopErrorCode::NotFound,        // no backend id
                (None, _, false, true) => RopErrorCode::NotFound, // backend id but no JMAP client
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
            // §2.2.7.6.1: LogonId · InputHandleIndex. Identical send path to
            // RopSubmitMessage on the gateway (both drive EmailSubmission/set
            // against the saved draft referenced by the input handle); the
            // difference is purely client-side (TransportSend carries a
            // completion callback property set which we return empty).
            let _logon = cur.take_u8()?;
            let req = RopTransportSendRequest::decode_after_ropid(cur)?;
            let (backend_id, is_new) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message { backend_id, is_new, .. } => (backend_id.clone(), *is_new),
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
            // ResponseHandleIndex byte — a phantom read here would steal the
            // low byte of PropertyValueSize and abort every SetProperties with
            // InvalidParameter. The shared success/failure envelope echoes
            // the InputHandleIndex as HandleIndex (qodo #1, cubic #16/#22).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopSetPropertiesRequest::decode(cur)?;
            // Resolve the Message handle (backend_id = JMAP email id,
            // mailbox_id = the JMAP mailbox the email lives in, used to range
            // the published modification event).
            let (backend_id, mailbox_id) = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message { backend_id, mailbox_id, .. } => {
                        (backend_id.clone(), mailbox_id.clone())
                    }
                    _ => (String::new(), String::new()),
                })
                .unwrap_or((String::new(), String::new()));
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
                        match jc.update_email_checked(&account_id, &update, username, pw).await {
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
            // #30, audit §2e).
            if return_value == RopErrorCode::Success {
                publish_item_modified(
                    subscription_manager,
                    username,
                    &mailbox_id,
                    &backend_id,
                );
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
            // InputHandleIndex — NO ResponseHandleIndex byte (same P0 fix as
            // SetProperties; a phantom read here steals the low byte of
            // PropertyTagCount and aborts every DeleteProperties). The
            // envelope echoes InputHandleIndex as HandleIndex
            // (qodo #1, cubic #16/#22).
            let _logon = cur.take_u8()?;
            let input_handle_index = cur.take_u8()?;
            let req = RopDeletePropertiesRequest::decode(cur)?;
            let (backend_id, mailbox_id) = sessions
                .with_handle(session_id, input_handle_index, |h| match h {
                    Handle::Message { backend_id, mailbox_id, .. } => {
                        (backend_id.clone(), mailbox_id.clone())
                    }
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
                        match jc.update_email_checked(&account_id, &update, username, pw).await {
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
                publish_item_modified(
                    subscription_manager,
                    username,
                    &mailbox_id,
                    &backend_id,
                );
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
            // MoveCopy convention — the dispatcher consumes LogonId, the
            // decoder consumes handles + body — so coderabbit #5 is INVALID;
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
                    Handle::Message { backend_id, mailbox_id, .. } => {
                        (backend_id.clone(), mailbox_id.clone())
                    }
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
            // (2.2.8.12.2 — problems report per-property issues; the
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

            let return_value: RopErrorCode = match (jmap, password, src_id.as_str(), dst_id.as_str()) {
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
                                if !excluded_subject
                                    && let Some(subj) = src_email.subject.as_ref()
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
                                    let update =
                                        serde_json::json!({ dst: serde_json::Value::Object(patch) });
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
                publish_item_modified(
                    subscription_manager,
                    username,
                    &dst_mailbox_id,
                    &dst_id,
                );
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
            // §2.2.6.13.1: header-only request (RopId · LogonId ·
            // InputHandleIndex · OutputHandleIndex). The dispatcher consumed
            // the RopId; decode the trailing LogonId · Input · Output via
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
            // attachment's bytes (RopWriteStream on PR_ATTACH_DATA_BIN →
            // JMAP Blob/upload → Email/set patching attachments[]) is not yet
            // wired. Surface `NoSupport` so a client gets a typed error rather
            // than `NotFound`, and do not allocate a phantom attachment
            // handle the save could not commit. (Audit §2a write-back follow-up)
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
            // §2.2.6.14.1: RopId · LogonId · InputHandleIndex ·
            // AttachmentID(4 LE). The dispatcher consumed the RopId and
            // LogonId, so decode_after_ropid reads InputHandleIndex +
            // AttachmentID.
            let req = RopDeleteAttachmentRequest::decode_after_ropid(cur)?;
            // The input handle MUST be a mail Message; AttachmentID is the
            // PR_ATTACH_NUM the client wants to remove.
            let (kind, email_id) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Message { backend_id, kind, .. } => (*kind, backend_id.clone()),
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
            // path unions that store — tracked with the body write-back bridge.)
            let _ = attachment_manager;
            RopDeleteAttachmentResponse {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::NoSupport,
            }
            .encode(out);
        }
        RopId::ROP_SAVE_CHANGES_ATTACHMENT => {
            // §2.2.6.15.1: RopId · LogonId · ResponseHandleIndex
            // · InputHandleIndex · SaveFlags(1). The dispatcher consumed the
            // RopId and LogonId, so decode_after_ropid reads
            // ResponseHandleIndex · InputHandleIndex · SaveFlags.
            let req = RopSaveChangesAttachmentRequest::decode_after_ropid(cur)?;
            // The input handle MUST be an Attachment handle (opened by
            // RopCreateAttachment). A JMAP-native attachment opened by
            // RopOpenAttachment is immutable, so SaveChangesAttachment on it
            // is an idempotent Success (the client sometimes re-saves after a
            // read-only OpenAttachment); a not-yet-persisted MAPI-created
            // attachment needs the body write-back bridge (Blob/upload →
            // Email/set) which is not wired, so it surfaces `NoSupport`.
            let (is_jmap_native, email_id) = sessions
                .with_handle(session_id, req.input_handle_index, |h| match h {
                    Handle::Attachment { is_new, email_id, .. } => (!is_new, email_id.clone()),
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
            // MAPI-created attachment pending the body write-back bridge.
            let _ = attachment_manager;
            let _ = email_id;
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
            let req = RopOpenStreamRequest::decode_body(cur, h4.input_handle_index, h4.output_handle_index)?;
            // Resolve the owning object from the input handle. A Mail Message
            // handle streams its body properties; an Attachment handle (opened
            // by `RopOpenAttachment`/`RopCreateAttachment`) streams that one
            // attachment's `PR_ATTACH_DATA_BIN` directly off the handle's cached
            // `blob_id`, so a message carrying several attachments streams the
            // *correct* one (the message-scoped `email_attachment_blob` path
            // below intentionally fails closed for >1 attachment).
            enum StreamOwner {
                Message(String, String, FolderKind),
                Attachment(String, String, FolderKind, String),
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
                        ..
                    } => StreamOwner::Attachment(
                        email_id.clone(),
                        mailbox_id.clone(),
                        *kind,
                        blob_id.clone(),
                    ),
                    _ => StreamOwner::Other,
                })
                .unwrap_or(StreamOwner::Other);
            // Fast path: an Attachment handle streaming PR_ATTACH_DATA_BIN (the
            // usual New Outlook "download this attachment" flow). The handle
            // already carries the JMAP blob id (Stalwart-assigned, captured at
            // `RopOpenAttachment` for a JMAP-native attachment or at
            // `RopSaveChangesAttachment` after `Blob/upload` for a MAPI-created
            // one), so we install a read-only Stream resolved to that specific
            // blob without re-reading the message body. `known_len` is `None`
            // (the precise length lands on the first `RopReadStream` when the
            // blob is materialised), so `OpenStream`'s StreamSize is 0 and the
            // client pages through the bytes the download returns.
            if let StreamOwner::Attachment(email_id, mailbox_id, kind, blob_id) = &owner
                && kind == &FolderKind::Mail
                && !email_id.is_empty()
            {
                let packed = if blob_id.is_empty() {
                    email_id.clone()
                } else {
                    format!("{email_id}\x1F{blob_id}")
                };
                // For a JMAP-native attachment the precise byte length is only
                // known once the blob is downloaded (the JMAP `attachments[]`
                // size is available on the message, but re-fetching the whole
                // Email/get at OpenStream time would add a round-trip per
                // attachment); record `None` so the first `RopReadStream`
                // materialises the blob and the stream size becomes exact then.
                // The message-scoped path above already provides the
                // optimised `known_len`-from-`attachments[].size` flow when
                // the client streams a body/attachment off the Message handle.
                let known_len: Option<u64> = None;
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
                RopOpenStreamSuccess {
                    output_handle_index: req.output_handle_index,
                    return_value: RopErrorCode::Success,
                    stream_size: 0,
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
                                        match store::email_attachment_blob(&e, &req.property_tag)
                                        {
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
                            Ok(None) => {
                                (RopErrorCode::NotFound, String::new(), None, false, None)
                            }
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
                materialise_attachment_blob(jmap, password, username, &stream_meta.0, cfg)
                    .await
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
            // §2.2.9.3.1: LogonId · InputHandleIndex · DataSize(2) · Data.
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
            // §2.2.9.8.1: LogonId · InputHandleIndex · Origin(1) · Offset(8).
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
            let max = 0x8000_0000u64.min(u64::try_from(cfg.max_attachment_bytes()).unwrap_or(0x8000_0000));
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
                    Handle::Stream { data, known_len, .. } => {
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
            let size = if len == 0 { u32::try_from(known).unwrap_or(0) } else { u32::try_from(len).unwrap_or(u32::MAX) };
            RopGetStreamSizeSuccess {
                input_handle_index: req.input_handle_index,
                return_value: RopErrorCode::Success,
                stream_size: size,
            }
            .encode(out);
        }
        RopId::ROP_COMMIT_STREAM => {
            // §2.2.9.5.1: LogonId · InputHandleIndex.
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
        _ => {
            // Unknown/unimplemented: emit a ROP-level NotFound so the client
            // falls back. Cursor is advanced only past the RopId byte here;
            // remaining body bytes are consumed best-effort by skipping to
            // end (Outlook re-issues the unacked ROPs).
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
///   handler mapped every `Ok` to `Success` — masking real failures as
///   success (qodo #3/#5, cubic #23). The `label` string is included in the
///   log so partial-failure traces name which ROP the update served.
fn outcome_to_code(outcome: crate::jmap::EmailSetOutcome, label: &'static str) -> RopErrorCode {
    if let Some(desc) = outcome.method_error {
        tracing::warn!(error = %desc, "%{label}: JMAP method rejected update");
        return RopErrorCode::DiskError;
    }
    if !outcome.not_updated.is_empty() {
        // Per-RFC-8621 §4.5 a `notUpdated` entry signals the server refused
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
/// §2e). No-op when the gateway was built without a manager wired (unit-test
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

/// Resolve the JMAP mailbox id with `role == "drafts"` (RFC 8621 §5.1) for the
/// account, used when a `RopCreateMessage` handle did not carry a parent
/// mailbox id (the client opened the synthetic root). Falls back to the empty
/// string on failure — the JMAP server will reject the create with no mailbox,
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

/// Pull the contents of a folder as bare row ids (Phase-1 minimal).
async fn fetch_contents_rows(
    jc: &crate::jmap::JmapClient,
    username: &str,
    password: &secrecy::SecretString,
    mailbox_id: &str,
    kind: FolderKind,
) -> Vec<crate::mapi::session::TableRow> {
    let _ = kind;
    let account_id = match jc.get_account_id(username, password).await {
        Ok(a) => a,
        Err(_) => return Vec::new(),
    };
    let params = crate::jmap::QueryEmailsParams {
        account_id: &account_id,
        filter: Some(serde_json::json!({"inMailbox": mailbox_id})),
        sort: None,
        position: 0,
        limit: 200,
        username,
        password,
    };
    let list = match jc.query_emails(params).await {
        Ok(l) => l,
        Err(_) => return Vec::new(),
    };
    list.emails
        .into_iter()
        .map(|e| {
            let jid = e.id.clone().unwrap_or_default();
            let source: std::sync::Arc<dyn std::any::Any + Send + Sync> = std::sync::Arc::new(e);
            crate::mapi::session::TableRow {
                row_id: store::message_id_from_jmap(&jid),
                cells: Vec::new(),
                source: Some(source),
            }
        })
        .collect()
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
/// (the property is unknown / unsupported for this object) — delegates to the
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

/// `Disconnect` RPC: drop the session if present, return 0.
async fn handle_disconnect(req: MapiRequest, state: &MapiState) -> MapiResponse {
    // Per MS-OXCMAPIHTTP §3.2.5.5 the Session Context is identified by the
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
    // The full transport→logon plumbing is exercised in the integration
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
        // RopRelease wire: RopId(0x01) · LogonId(0) · InputHandleIndex(3)
        let body: Vec<u8> = vec![0x01, 0, 3];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
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
                },
            );
        });

        // Column set: PR_SUBJECT (PtypString=0x001F + id 0x0037) + PR_MID (PtypInteger64=0x0014 + id 0x6748).
        // MS-OXCDATA §2.9 PropertyTag wire order is PropertyType(2 LE) THEN PropertyId(2 LE).
        let set_columns_body: Vec<u8> = {
            let mut b = Vec::new();
            // RopId(0x06) · LogonId(0) · InputHandleIndex(5) · SetColumnFlags(0)
            // · PropertyTagCount(2 LE = 2) · [tag1][tag2]
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

        // QueryRows: RopId(0x15) · LogonId(0) · InputHandleIndex(5) ·
        // QueryRowsFlags(0) · ForwardRead(0) · RowCount(2 LE = 1)
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

        // Response: RopId(0x15) · InputHandleIndex(5) · ReturnValue(4 LE=0)
        // · Origin(1) · RowCount(2 LE=1) · flag(1)=0 · <subject cell> · <mid cell>
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
        // PR_SUBJECT cell per MS-OXCDATA §2.11.2.1: UTF-16LE code units
        // INCLUDING the 0x0000 terminator, with NO length prefix.
        // "Hello MAPI" is 10 code units → 22 bytes (no length word).
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

    /// Regression: `RopOpenFolder` wire (`RopId(0x02)·LogonId·InputHandle·OutputHandle
    /// ·FolderId(8 LE)·OpenModeFlags(1)`) must reach the dispatcher's
    /// folder-open path with the **real** input/output-handle indices and not
    /// bytes shifted by one. A previous implementation called
    /// `RopHeader4::decode` after the dispatcher had already consumed the
    /// leading `RopId` byte, so `RopHeader4` re-read the LogonId as the RopId,
    /// the InputHandle as the LogonId, the OutputHandle as the InputHandle,
    /// and the high byte of FolderId as the OutputHandle — corrupting both
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

        // Wire: RopId·LogonId·Input·Output·FolderId(8 LE)·OpenModeFlags(1) = 13 bytes.
        let mut body = vec![0x02u8, /*LogonId*/ 0x00, INPUT_HANDLE, OUTPUT_HANDLE];
        body.extend_from_slice(&FOLDER_ID.to_le_bytes());
        body.push(0); // OpenModeFlags (Open mode = 0).

        let mut cur = crate::mapi::rops::Buf::new(&body);
        cur.take_u8().ok(); // consume RopId — matches the runtime dispatcher.
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

        // RopOpenFolderSuccess: RopId(0x02) · OutputHandleIndex · ReturnValue(4 LE)
        // · HasRules(1) · IsGhosted(1).
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
    /// client's OutputHandleIndex and echo `RopId(0x06) · OutputHandleIndex
    /// · Success · HasMessageId=1 · MessageId(8 LE=0 placeholder)`, even with
    /// no JMAP backend configured — the draft is not persisted until the
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
        // Wire: RopId(0x06) · LogonId(0) · InputHandle(3) · OutputHandle(9)
        // · CodePageId(2 LE=0) · FolderId(8 LE) · AssociatedFlag(1=0) = 13 bytes.
        let mut body = vec![0x06u8, 0x00, INPUT_HANDLE, OUTPUT_HANDLE];
        body.extend_from_slice(&0u16.to_le_bytes()); // CodePageId
        body.extend_from_slice(&FOLDER_ID.to_le_bytes());
        body.push(0); // AssociatedFlag
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            password: None,
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        // RopCreateMessageSuccess: RopId · OutputHandleIndex · RV(4) ·
        // HasMessageId(1) · MessageId(8 LE placeholder).
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
        // No jmap_base → jmap backend is None at dispatch time.
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
        // Wire: RopId(0x0C) · LogonId(0) · ResponseHandleIndex(2)
        // · InputHandleIndex(4) · SaveFlags(0).
        let body: Vec<u8> = vec![0x0C, 0, RESPONSE_HANDLE, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        assert_eq!(payload[0], 0x0C, "echoed RopId");
        // RopSaveChangesMessageSuccess: RopId · ResponseHandleIndex · RV(4) ·
        // InputHandleIndex · MessageId(8).
        assert_eq!(payload[1], RESPONSE_HANDLE, "response handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NotFound,
            "no JMAP backend ⇒ NotFound, not silent Success"
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
        // Wire: RopId(0x1E) · LogonId(0) · InputHandle(1) · WantAsynchronous(0)
        // · NotifyNonRead(0) · MessageIdCount(2 LE=1) · MessageId(8 LE=42).
        let mut body = vec![0x1Eu8, 0, INPUT_HANDLE, 0, 0];
        body.extend_from_slice(&1u16.to_le_bytes());
        body.extend_from_slice(&42u64.to_le_bytes());
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
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
            "non-mail folder ⇒ NoSupport"
        );
        assert_eq!(payload[6], 0, "PartialCompletion=0");
    }

    /// `RopSubmitMessage` against an unsaved (`is_new`) draft must emit
    /// `InvalidParameter` rather than attempting to submit a draft with no
    /// backend id — guarding the EmailSubmission path against an envelope
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
        // Wire: RopId(0x32) · LogonId(0) · InputHandle(7) · SubmitFlags(0).
        let body: Vec<u8> = vec![0x32, 0, INPUT_HANDLE, 0];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
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
            "unsaved draft ⇒ InvalidParameter"
        );
    }

    /// `RopTransportSend` failure (no JMAP backend) must emit the FAILURE
    /// response shape `RopId · InputHandleIndex · ReturnValue(4)` — NOT the
    /// success shape that adds `NoPropertiesReturned · PropertyValueCount`.
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
        // Wire: RopId(0x4A) · LogonId(0) · InputHandle(5).
        let body: Vec<u8> = vec![0x4A, 0, INPUT_HANDLE];
        let req = MapiRequest {
            kind: RpcKind::Mailbox(MapiRequestType::Execute),
            request_id: "{G}:1".into(),
            client_application: None,
            client_info: Some(format!("{{{}}}:0", sid.as_hyphenated())),
            password: Some("pw".into()),
            cookies: Vec::new(),
            body,
        };
        let resp = handle(req, &state).await;
        assert_eq!(resp.code, ResponseCode::Success);
        let (_status, _h, _ct, body_out) = resp.render();
        let payload = &body_out[4..];
        // Failure envelope is exactly 6 bytes: RopId · InputHandleIndex · RV(4).
        assert_eq!(payload.len(), 6, "transport-send failure envelope length");
        assert_eq!(payload[0], 0x4A, "echoed RopId");
        assert_eq!(payload[1], INPUT_HANDLE, "input handle echoed");
        let rv = u32::from_le_bytes([payload[2], payload[3], payload[4], payload[5]]);
        assert_eq!(
            RopErrorCode::from_u32(rv),
            RopErrorCode::NotFound,
            "no JMAP backend ⇒ NotFound"
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
    /// emit `NoSupport` and echo the SOURCE handle index (per §2.2.4.6.2 the
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
        // Wire: RopId(0x33) · LogonId(0) · SourceHandle(2) · DestHandle(8)
        // · MessageIdCount(2 LE=1) · MessageId(8 LE=7) · WantAsync(0) · WantCopy(0).
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
        // publishing MUST be a safe no-op — verifies the `Option` plumbing
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
        let sid = state.sessions.create(crate::mapi::session::SessionPrincipal {
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
        // ReadStream: RopId(0x2C) · LogonId(0) · InputHandleIndex(4) · ByteCount(5)
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
        // SeekStream: RopId(0x2E) · LogonId(0) · InputHandleIndex(1) ·
        // Origin(0x00=begin) · Offset(8 LE = 3)
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
        // GetStreamSize: RopId(0x5E) · LogonId(0) · InputHandleIndex(2)
        let out = dispatch(&state, &sid, &[0x5E, 0, 2]).await;
        assert_eq!(out[0], 0x5E);
        let size = u32::from_le_bytes([out[6], out[7], out[8], out[9]]);
        assert_eq!(size, 10);
    }

    #[tokio::test]
    async fn write_stream_overwrites_and_extends() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 3, b"hello".to_vec(), 0, false);
        // WriteStream "XY" at cursor 0 → "XYllo", cursor=2.
        let mut body: Vec<u8> = vec![0x2D, 0, 3, 2, 0];
        body.extend_from_slice(b"XY");
        let out = dispatch(&state, &sid, &body).await;
        assert_eq!(out[0], 0x2D);
        assert_eq!(u16::from_le_bytes([out[6], out[7]]), 2);
        // Write "Z" at cursor 2 → "XYZlo".
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
        assert_eq!(u32::from_le_bytes([outgss[6], outgss[7], outgss[8], outgss[9]]), 5);
        let outrs = dispatch(&state, &sid, &[0x2C, 0, 3, 0xFF, 0xFF]).await;
        let ds = u16::from_le_bytes([outrs[6], outrs[7]]);
        assert_eq!(String::from_utf8_lossy(&outrs[8..8 + ds as usize]), "XYZlo");
    }

    #[tokio::test]
    async fn set_stream_size_truncates_and_clamps_cursor() {
        let (state, sid) = state_with_session();
        install_stream(&state, &sid, 5, b"abcdefghij".to_vec(), 9, false);
        // SetStreamSize: RopId(0x2F) · LogonId(0) · InputHandleIndex(5) · StreamSize(8=4)
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
}
