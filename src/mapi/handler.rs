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
    Buf, DecodeError, RopCreateMessageRequest, RopCreateMessageSuccess, RopDeleteMessagesRequest,
    RopDeleteMessagesResponse, RopErrorCode, RopErrorResponse, RopGetPropertiesAllRequest,
    RopGetPropertiesSpecificRequest, RopGetStatusRequest, RopHeader4, RopId, RopLogonRequest,
    RopLogonSuccess, RopMoveCopyMessagesRequest, RopMoveCopyMessagesResponse, RopOpenTableRequest,
    RopQueryRowsRequest, RopReleaseRequest, RopSaveChangesMessageRequest,
    RopSaveChangesMessageSuccess, RopSetColumnsRequest, RopSetMessageReadFlagRequest,
    RopSubmitMessageRequest, RopSubmitMessageResponse, RopTransportSendFailure,
    RopTransportSendRequest, RopTransportSendSuccess,
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
}

impl MapiState {
    pub fn new(cfg: Config, auth: std::sync::Arc<AuthVerifier>) -> Self {
        Self {
            cfg,
            auth,
            sessions: SessionManager::new(),
        }
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
#[derive(Clone, Copy)]
enum HandleShape {
    Message,
    Folder,
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
            let outcome: RopErrorCode;
            let saved_mid: u64;
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
                                        Ok(()) => {
                                            outcome = RopErrorCode::Success;
                                            partial = if found == req.message_ids.len() {
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
                (None, _, _) => {
                    outcome = RopErrorCode::NotFound;
                    partial = 0;
                }
                (_, None, _) => {
                    outcome = RopErrorCode::AccessDenied;
                    partial = 0;
                }
                (_, _, false) => {
                    outcome = RopErrorCode::NoSupport;
                    partial = 0;
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
                                    partial = 0;
                                } else if req.want_copy != 0 {
                                    match jc
                                        .copy_emails(&account_id, &jids, &dest_backend, username, pw)
                                        .await
                                    {
                                        Ok(n) => {
                                            outcome = RopErrorCode::Success;
                                            partial = if n == jids.len() { 0 } else { 1 };
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
                                            partial = if n == jids.len() { 0 } else { 1 };
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
                (None, _, _, _) => {
                    outcome = RopErrorCode::NotFound;
                    partial = 0;
                }
                (_, None, _, _) => {
                    outcome = RopErrorCode::AccessDenied;
                    partial = 0;
                }
                (_, _, false, _) | (_, _, _, false) => {
                    outcome = RopErrorCode::NoSupport;
                    partial = 0;
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
                (None, _, _, _) => RopErrorCode::NotFound,
                (_, None, _, _) => RopErrorCode::AccessDenied,
                (_, _, true, _) => RopErrorCode::InvalidParameter, // unsaved draft
                (_, _, _, false) => RopErrorCode::NotFound,        // no backend id
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
                (None, _, _, _) => RopErrorCode::NotFound,
                (_, None, _, _) => RopErrorCode::AccessDenied,
                (_, _, true, _) => RopErrorCode::InvalidParameter,
                (_, _, _, false) => RopErrorCode::NotFound,
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
    let _ = cfg;
    let _ = logon_id;
    Ok(())
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
}
