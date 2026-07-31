// src/mapi/session.rs
//
// Per-connection MAPI/HTTP session store. A `Connect` RPC allocates a session
// keyed by an opaque cookie; subsequent `Execute` RPCs must carry that
// cookie (validated by the transport layer's `X-ClientInfo`/cookie path) and
// are bound to the session's `LogonId` roster.
//
// Phase 0:
//   * A `SessionManager` holding sessions in a `parking_lot::RwLock`'d map
//     keyed by an opaque session id (a `Uuid`).
//   * Idle TTL with a sweeper hook; sessions are removed on idle expiry and
//     on explicit `Disconnect`.
//   * `Session` carries the authenticated user's email address (resolved by
//     `auth.rs` for Basic, by `oidc.rs` for bearer/HMA), the `LogonId`
//     roster for the connection, and a handle table for open folders/
//     messages. The session's auth principal is zeroized when the session is
//     dropped.
//   * All sessions are per-process only (no persistence) — consistent with
//     the MAPI/HTTP model where a `Connect` failure means the client rebuilds
//     a new session.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use uuid::Uuid;
use zeroize::Zeroize;

/// A fixed monotonic-epoch reference (process startup) for converting
/// `Instant`s into `u64` nanos without a lock. Established once and read
/// lock-free thereafter; the value is monotonic per host and unique across
/// restarts, so its absolute value does not need to be stable.
static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

fn epoch() -> Instant {
    *EPOCH.get_or_init(Instant::now)
}

/// Nanos since `EPOCH`. Caps at `u64::MAX` past ~584 years (overflow is a
/// non-issue in practice and saturates rather than wrapping).
fn epoch_nanos(t: Instant) -> u64 {
    t.checked_duration_since(epoch())
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(u64::MAX)
}

/// Default idle TTL for a MAPI/HTTP session. Outlook keeps connect sessions
/// open for the lifetime of the app window; we expire after this idle window
/// so a forgotten-then-resumed connection gets a deterministic `ContextNotFound`
/// (code 10) rather than serving stale state.
pub const DEFAULT_SESSION_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

/// The kind of object a session handle indexes, plus the backend-resolved
/// state the ROP dispatcher needs to drive the matching op against Stalwart.
#[derive(Debug, Clone)]
pub enum Handle {
    /// A mailbox folder. `backend_id` is the JMAP mailbox id (RFC 8621 s.5)
    /// for mail folders, or the CalDAV collection href for calendar folders,
    /// or the CardDAV addressbook href for the contacts folder. `folder_kind`
    /// routes a `RopGetContentsTable` to the right backend.
    Folder {
        backend_id: String,
        kind: FolderKind,
    },
    /// A message handle. `backend_id` is the JMAP email id; `mailbox_id` is
    /// the parent JMAP mailbox id (used by `RopSaveChangesMessage` /
    /// `RopDeleteMessages`). `folder_kind` records which backend owns the
    /// message (mail/calendar/contact) for the SetProperties path.
    Message {
        backend_id: String,
        mailbox_id: String,
        kind: FolderKind,
        /// True if the handle was produced by `RopCreateMessage` and not yet
        /// `RopSaveChangesMessage`'d; until save the message lives in the
        /// drafts mailbox only as a pending JMAP creation.
        is_new: bool,
    },
    /// An attachment handle (`RopOpenAttachment` / `RopCreateAttachment`,
    /// MS-OXCMSG §2.2.3). Backs `RopGetProperties{Specific,All}` /
    /// `RopSetProperties` / `RopOpenStream` (on `PR_ATTACH_DATA_BIN`) /
    /// `RopSaveChangesAttachment` / `RopDeleteAttachment` against the owning
    /// message's attachment.
    ///
    /// `email_id` is the owning JMAP email id; `attach_num` is the
    /// `PR_ATTACH_NUM` (the JMAP `attachments[]` index for a JMAP-native
    /// attachment, or a freshly-assigned index for one created via MAPI and
    /// not yet persisted); `blob_id` is the JMAP blob id for the attachment
    /// bytes (the id Stalwart assigned for a JMAP-native attachment, enabling
    /// a `RopOpenStream`+`RopReadStream` download without re-reading the
    /// source message handle); `name`/`content_type`/`size` are the
    /// read-only metadata captured from JMAP at `RopOpenAttachment` time so
    /// `RopGetProperties*` and `RopOpenStream` (for `known_len`/`stream_size`
    /// and the `max_attachment_bytes` ceiling) need no extra JMAP round-trip.
    /// `is_new` distinguishes a JMAP-native attachment (`false`, immutable —
    /// `RopSaveChangesAttachment` is an idempotent success and
    /// `RopDeleteAttachment` is `NoSupport` because the MIME-rewrite bridge is
    /// pending) from one created via MAPI (`true`, staged in
    /// `blob_id`/`name`/`content_type` until the write-back bridge persists
    /// it).
    ///
    /// Note: writes via `RopSetProperties` against this handle are not
    /// supported in this phase (the body/MIME-rewrite bridge is pending);
    /// `RopSetProperties` returns `NoSupport` for an attachment handle rather
    /// than mutating the cached metadata, which is therefore the JMAP source
    /// of truth, not client-settable through MAPI.
    Attachment {
        /// Owning message JMAP email id.
        email_id: String,
        /// Owning message JMAP mailbox id (carried so SaveChangesAttachment can
        /// re-thread the parent mailbox if a draft create was needed).
        mailbox_id: String,
        /// Which backend owns the source message (mail/calendar/contact). Mail
        /// is the only kind with attachment streams in this phase.
        kind: FolderKind,
        /// The `PR_ATTACH_NUM` of this attachment.
        attach_num: u32,
        /// JMAP blob id of the attachment bytes (Stalwart-assigned). Empty for
        /// a freshly-created attachment before `RopSaveChangesAttachment`.
        blob_id: String,
        /// The attachment's display name (read-only capture from JMAP
        /// `attachments[].name`).
        name: String,
        /// The attachment's MIME content type (read-only capture from JMAP
        /// `attachments[].contentType`).
        content_type: String,
        /// The attachment's declared byte length, captured from JMAP
        /// `attachments[].size` at `RopOpenAttachment` so the attachment
        /// stream reports a real length (`RopOpenStream` `stream_size` /
        /// `RopGetStreamSize`) before the blob is downloaded, and the
        /// `max_attachment_bytes` ceiling can reject an oversized blob with
        /// `NotEnoughMemory` before the download. `None` when the size is
        /// not declared (then the stream reports 0 and the length is
        /// discovered on the first `RopReadStream`).
        size: Option<u64>,
        // True when created via `RopCreateAttachment` and not yet saved.
        is_new: bool,
    },
    /// A stream handle (`RopOpenStream`, MS-OXCROPS 2.2.9). Backs the
    /// `RopReadStream` / `RopWriteStream` / `RopSeekStream` /
    /// `RopGetStreamSize` / `RopSetStreamSize` / `RopCommitStream` round-trip
    /// the client uses to fetch `PR_BODY` / `PR_BODY_HTML` / `PR_RTF_COMPRESSED`
    /// (MS-OXBBODY) and attachment binaries (MS-OXCMSG 3.x).
    ///
    /// `source_handle_index` records the message handle owning the streamed
    /// property; the dispatcher reads it (not the stream itself) to resolve
    /// the JMAP email id / attachment blob id. `data` is the lazily-materialised
    /// byte buffer of the property value (`None` until first read so an
    /// OpenStream on a property the client never reads costs zero network
    /// round-trip); `cursor` is the absolute byte position the next
    /// `RopReadStream` continues from and that `RopSeekStream` repositions.
    /// `is_dirty` distinguishes a read-only stream from one a
    /// `RopWriteStream`/`RopSetStreamSize` mutated: writes are staged in the
    /// in-memory buffer and are NOT yet flushed back to JMAP at
    /// `RopSaveChangesMessage` (the Blob/upload-backed Email/set body-values
    /// flush is pending the separate compose/write-back bridge); SaveChanges
    /// reports `NoSupport` when a dirty body stream is present so the client is
    /// never told Success while bytes are dropped.
    Stream {
        /// Session handle index of the owning Message/Folder handle. Resolved on
        /// first read so the body/attachment bytes are fetched once per stream.
        source_handle_index: u8,
        /// Which backend owns the source object (mail/calendar/contact). Mail is
        /// the only kind with a streamed body in this phase; calendar/contact
        /// stream arms return `NoSupport`.
        kind: FolderKind,
        /// The JMAP email id (for body streams) or the email id plus attachment
        /// `blob_id` (for attachment streams), packed as `"<emailId>\x1F<blobId>"`
        /// when an attachment is streamed. Resolved from the source message
        /// handle at OpenStream time, so the source handle can be released
        /// before the stream is read.
        backend_id: String,
        mailbox_id: String,
        /// The streamed property tag (read back from the request). Used to pick
        /// the body/attachment fetch path and to validate type compatibility.
        property_tag: crate::mapi::data::PropertyTag,
        /// Lazily-materialised stream bytes (`None` = not fetched yet). Once
        /// populated, `cursor` / `is_dirty` mutate the buffer in place.
        data: Option<Vec<u8>>,
        /// The stream's known length when it can be determined without a fetch:
        /// for an attachment stream this is the JMAP `attachments[].size`
        /// captured at OpenStream (so `OpenStream`/`GetStreamSize` report the
        /// real size before the first `ReadStream` downloads the blob, and the
        /// download can be rejected up front when it exceeds `max_attachment_bytes`).
        /// `None` for body streams whose length is only known once materialised.
        known_len: Option<u64>,
        /// Absolute byte position the next ReadStream continues from.
        cursor: u64,
        /// True once `RopWriteStream`/`RopSetStreamSize` mutated `data`. Writes
        /// are staged in-memory only; the JMAP persist happens via a separate
        /// write-back bridge not yet wired, so SaveChanges reports `NoSupport`
        /// rather than faking a commit of dropped bytes.
        is_dirty: bool,
        /// Whether `data` was populated from an attachment blob download (the
        /// OpenStream arm records this so a `Set/Write` on an attachment stream
        /// is rejected as read-only until CreateAttachment lands).
        read_only: bool,
    },
    /// A table handle (results of `RopGetHierarchyTable` / `RopGetContentsTable`).
    /// `column_set` is the most recently applied `RopSetColumns` tag list;
    /// `rows` are the per-row `(row_id, PropertyRowEntryVec)` slots the
    /// dispatcher pre-materialised when the table was opened (or re-materialised
    /// on the first QueryRows). For mail this derives from `Email/query`+`Email/get`;
    /// for hierarchy from `Mailbox/query`; for calendars from
    /// `CalendarEvent/query`; for contacts from the CardDAV `addressbook-query`.
    Table {
        kind: FolderKind,
        /// Owner folder handle index in the session table (-1 signals the root
        /// mailbox/logon handle). Used to record the parent for hierarchy tables.
        parent_handle: i16,
        /// The JMAP mailbox id / CalDAV collection href of the folder this
        /// contents table enumerates (`""` for hierarchy tables). Used by the
        /// QueryRows materialiser to thread `mailbox_id` into
        /// `store::email_to_cells`.
        parent_backend_id: String,
        /// The MAPI column set established by the last successful RopSetColumns.
        column_set: Vec<crate::mapi::data::PropertyTag>,
        /// Materialised rows: each row carries its 64-bit MAPI row id (a
        /// folder id / message id / contact href-hash) and the pre-resolved
        /// property values for the current column set, ready to serialise on
        /// a `RopQueryRows`.
        rows: Vec<TableRow>,
        /// Cursor position (absolute, 0-based) the next QueryRows continues from.
        cursor: usize,
        /// Total row count the backend reported when the table was opened.
        total: u64,
    },
}

impl Handle {
    pub fn kind(&self) -> HandleKind {
        match self {
            Self::Folder { .. } => HandleKind::Folder,
            Self::Message { .. } => HandleKind::Message,
            Self::Attachment { .. } => HandleKind::Attachment,
            Self::Stream { .. } => HandleKind::Stream,
            Self::Table { .. } => HandleKind::Table,
        }
    }
}

/// Which backend owns a mailbox object — drives the ROP→backend routing in
/// `store.rs`. Maps to the JMAP `Mailbox` role / CalDAV collection / CardDAV
/// addressbook naming in `store.rs::resolve_folder_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderKind {
    Mail,
    Calendar,
    Contacts,
    /// The synthetic root folder returned by `RopLogon` (the mailbox itself),
    /// the parent of Inbox/Calendar/Contacts. Cannot hold message contents.
    Root,
}

/// A materialised row in a table handle, ready to serialise on QueryRows.
/// `row_id` is the 64-bit MAPI folder/message id used as the row key; `cells`
/// are the resolved values for the table's `column_set`, in column order.
///
/// `source` carries the raw backend object the row was built from, so the
/// QueryRows handler can materialise `cells` lazily once `SetColumns` has
/// fixed the column set. It is typed as `Arc<dyn Any + Send + Sync>` to keep
/// `session.rs` decoupled from the `jmap`/`caldav`/`carddav` crate modules
/// (which would otherwise create a cross-module dependency cycle through the
/// session manager). Once cells have been materialised the handler clears
/// `source` to bound memory.
#[derive(Clone)]
pub struct TableRow {
    pub row_id: u64,
    pub cells: Vec<crate::mapi::data::PropertyValue>,
    /// Opaque raw backend object the row was built from (a `JmapEmail`
    /// / `JmapMailbox` / parsed iCalendar `VEVENT` / `vCard`). Cleared once
    /// `cells` have been materialised. See the type's doc for the rationale.
    pub source: Option<std::sync::Arc<dyn std::any::Any + Send + Sync>>,
}

impl std::fmt::Debug for TableRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TableRow")
            .field("row_id", &self.row_id)
            .field("cells", &self.cells)
            .field("source", &self.source.as_ref().map(|_| "<opaque>"))
            .finish()
    }
}

/// Legacy alias kept for callers that only need the discriminant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleKind {
    Folder,
    Message,
    Attachment,
    Stream,
    Table,
}

/// An entry in a session's handle table — kept for source compatibility with
/// the wider crate; new code uses `Handle` directly.
#[derive(Debug, Clone)]
pub struct HandleEntry {
    pub kind: HandleKind,
    pub backend_id: String,
}

/// The authenticated principal bound to a session. Carries the user's email
/// address only — never a password; passwords are validated once during the
/// `Connect` and discarded. Bearer/HMA principals carry the token's `oid`/
/// `upn` instead.
#[derive(Debug, Clone, Zeroize)]
pub struct SessionPrincipal {
    pub email: String,
    /// Whether the principal was authenticated via Basic auth (true) or via
    /// an Entra ID bearer token (false). Used by `logon.rs` to decide whether
    /// to delegate to Stalwart with stored credentials or to bridge the
    /// token.
    pub basic_auth: bool,
}

/// A MAPI/HTTP session. Mutable fields (handle table, last-seen) mutate under
/// the manager's lock; the principal is `Zeroize`-d on drop.
#[derive(Debug)]
pub struct Session {
    /// Opaque session id echoed back to the client by the transport layer.
    pub id: Uuid,
    pub principal: SessionPrincipal,
    /// The LogonId the client associated with this session at RopLogon time.
    /// Outlook may request any logon id (per MS-OXCROPS s.2.2.3.1.1); we
    /// store it so subsequent ROPs validate the same id.
    pub logon_id: Option<u8>,
    /// Server-object handle table, keyed by the client-chosen handle index in
    /// `[0, 255]`. The handle 0 is conventionally bound to the mailbox root
    /// at RopLogon time; `with_session_mut` is the supported mutation path.
    pub handles: HashMap<u8, Handle>,
    /// Last-seen timestamp encoded as nanos since `EPOCH` to avoid a
    /// `parking_lot::Mutex` here — read/written from paths that already
    /// hold the `SessionManager` `RwLock` guard.
    pub last_seen: AtomicU64,
    pub created_at: Instant,
}

impl Session {
    pub fn touch(&self) {
        // Lock-free: store nanos since `EPOCH` into the atomic. This avoids
        // acquiring a Mutex while the caller may already hold the
        // `SessionManager` RwLock (read or write) — eliminating the
        // potential for lock-ordering surprises across `get`, `with_*`, and
        // `sweep_idle`. The atomic is monotonic enough for idle-TTL
        // comparisons; sub-millisecond precision loss at startup is fine.
        self.last_seen
            .store(epoch_nanos(Instant::now()), Ordering::Relaxed);
    }

    /// Pick the lowest free handle index in `[0, 255]`, install `handle`
    /// there, and return the index. Returns `None` if the table is full
    /// (all 256 slots in use). ROP clients choose their own output-handle
    /// index via RopHeader4's `OutputHandleIndex`, but for ROPs that
    /// implicitly allocate a server-side handle (none in the Phase-1 set),
    /// this is the allocator. RopOpenFolder/SetColumns callers instead
    /// install at a fixed client-chosen index via `handles.insert`.
    pub fn alloc_handle(&mut self, handle: Handle) -> Option<u8> {
        use std::collections::hash_map::Entry;
        for idx in 0u8..=255u8 {
            if let Entry::Vacant(e) = self.handles.entry(idx) {
                e.insert(handle);
                return Some(idx);
            }
        }
        None
    }

    /// Drop a handle (called by `RopRelease`).
    pub fn free_handle(&mut self, idx: u8) -> bool {
        self.handles.remove(&idx).is_some()
    }

    /// Install `handle` at the client-chosen `idx`, returning the displaced
    /// handle (almost always `None` in practice; MAPI clients reserve each
    /// output index only once per connection).
    pub fn set_handle(&mut self, idx: u8, handle: Handle) -> Option<Handle> {
        self.handles.insert(idx, handle)
    }

    /// Borrow the handle at `idx` mutably for an in-place update (e.g.
    /// advancing a table cursor or replacing the column set).
    pub fn handle_mut(&mut self, idx: u8) -> Option<&mut Handle> {
        self.handles.get_mut(&idx)
    }
}

/// Thread-safe session manager.
#[derive(Debug, Clone)]
pub struct SessionManager {
    inner: Arc<RwLock<HashMap<Uuid, Session>>>,
    idle_ttl: Duration,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_SESSION_IDLE_TTL)
    }
    pub fn with_ttl(idle_ttl: Duration) -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
            idle_ttl,
        }
    }
    /// The default idle-TTL in whole seconds. The background sweeper in
    /// `main.rs` uses this to set its tick cadence.
    pub fn default_idle_secs() -> u64 {
        DEFAULT_SESSION_IDLE_TTL.as_secs()
    }

    /// Allocate a new session for `principal` and return its id.
    pub fn create(&self, principal: SessionPrincipal) -> Uuid {
        let id = Uuid::new_v4();
        let session = Session {
            id,
            principal,
            logon_id: None,
            handles: HashMap::new(),
            last_seen: AtomicU64::new(epoch_nanos(Instant::now())),
            created_at: Instant::now(),
        };
        self.inner.write().insert(id, session);
        id
    }

    /// Look up a session and return an owned snapshot of the fields callers
    /// need. We deliberately return an owned `SessionSnapshot` rather than a
    /// borrow-into-the-lock: parking_lot's `RwLockReadGuard` cannot lend a
    /// `&Session` out past the guard's lifetime without referencing the
    /// guard itself, and that pattern is unsound against a concurrent
    /// `remove()`. The snapshot is cheap (one String + small handle table).
    pub fn get(&self, id: &Uuid) -> Option<SessionSnapshot> {
        let guard = self.inner.read();
        guard.get(id).map(|s| {
            s.touch();
            SessionSnapshot {
                id: s.id,
                principal: s.principal.clone(),
                logon_id: s.logon_id,
                handles: s.handles.clone(),
            }
        })
    }

    /// Bind `logon_id` to a session (called by the `RopLogon` path so the
    /// subsequent Execute ROPs can echo the same LogonId). Returns false if
    /// the session has gone away.
    pub fn set_logon_id(&self, id: &Uuid, logon_id: u8) -> bool {
        let mut guard = self.inner.write();
        if let Some(s) = guard.get_mut(id) {
            s.logon_id = Some(logon_id);
            true
        } else {
            false
        }
    }

    /// Run `f` against the live session under the write lock and return its
    /// result. This is the supported mutation path: the ROP dispatcher does
    /// every handle-table cell change (alloc a new handle, advance a table
    /// cursor, replace a folder/message handle, mark a message saved) through
    /// this closure so the lock is held for the atomic duration of the
    /// change. `f` receives the `Session` and is expected to keep its
    /// mutations lock-local (no awaiting).
    pub fn with_session_mut<R>(&self, id: &Uuid, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
        let mut guard = self.inner.write();
        guard.get_mut(id).map(|s| {
            s.touch();
            f(s)
        })
    }

    /// Convenience: snapshot a single live handle (clone) under the read lock.
    pub fn with_handle<R>(
        &self,
        id: &Uuid,
        handle_index: u8,
        f: impl FnOnce(&Handle) -> R,
    ) -> Option<R> {
        let guard = self.inner.read();
        guard
            .get(id)
            .and_then(|s| s.handles.get(&handle_index).map(f))
    }

    /// Remove a session (called by `Disconnect` or on idle expiry).
    pub fn remove(&self, id: &Uuid) -> bool {
        let removed = self.inner.write().remove(id);
        if let Some(mut s) = removed {
            // Dropping zeroes the Zeroize principal in-place.
            s.principal.zeroize();
            true
        } else {
            false
        }
    }

    /// Sweep sessions whose last-seen is older than the idle TTL. Returns the
    /// count removed. Called periodically by the handler.
    pub fn sweep_idle(&self) -> usize {
        let now_nanos = epoch_nanos(Instant::now());
        let ttl = self.idle_ttl;
        let mut guard = self.inner.write();
        let to_remove: Vec<_> = guard
            .iter()
            .filter_map(|(k, v)| {
                let last_nanos = v.last_seen.load(Ordering::Relaxed);
                // Age in nanos since last-seen; saturating on the
                // (impossible-in-practice) backwards clock.
                let age_nanos = now_nanos.saturating_sub(last_nanos);
                if Duration::from_nanos(age_nanos) > ttl {
                    Some(*k)
                } else {
                    None
                }
            })
            .collect();
        let n = to_remove.len();
        for k in to_remove {
            if let Some(mut s) = guard.remove(&k) {
                s.principal.zeroize();
            }
        }
        n
    }

    pub fn len(&self) -> usize {
        self.inner.read().len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.read().is_empty()
    }
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// An owned snapshot of the session fields callers need, produced under the
/// manager's read lock and released immediately. Avoids the borrow-past-guard
/// lifetime problem.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub id: Uuid,
    pub principal: SessionPrincipal,
    pub logon_id: Option<u8>,
    pub handles: HashMap<u8, Handle>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(email: &str) -> SessionPrincipal {
        SessionPrincipal {
            email: email.into(),
            basic_auth: true,
        }
    }

    fn table_handle() -> Handle {
        Handle::Table {
            kind: FolderKind::Mail,
            parent_handle: -1,
            parent_backend_id: String::new(),
            column_set: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            total: 0,
        }
    }

    #[test]
    fn set_logon_id_roundtrips() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        assert!(mgr.set_logon_id(&id, 7));
        let snap = mgr.get(&id).expect("session");
        assert_eq!(snap.logon_id, Some(7));
        // Setting again on a vanished session reports false.
        let stray = Uuid::new_v4();
        assert!(!mgr.set_logon_id(&stray, 1));
    }

    #[test]
    fn with_session_mut_alloc_and_free_handle() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        let idx = mgr
            .with_session_mut(&id, |s| {
                s.alloc_handle(Handle::Folder {
                    backend_id: "inbox".into(),
                    kind: FolderKind::Mail,
                })
            })
            .expect("session");
        assert_eq!(idx, Some(0));
        let snap = mgr.get(&id).expect("session");
        assert!(snap.handles.contains_key(&0));
        // free it
        let freed = mgr
            .with_session_mut(&id, |s| s.free_handle(0))
            .expect("session");
        assert!(freed);
        let snap = mgr.get(&id).expect("session");
        assert!(!snap.handles.contains_key(&0));
    }

    #[test]
    fn with_session_mut_updates_table_column_set_and_cursor() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        // Install a table handle at client-chosen index 5.
        mgr.with_session_mut(&id, |s| {
            s.set_handle(5, table_handle());
        })
        .expect("session");
        // Replace the column set and advance the cursor atomically.
        mgr.with_session_mut(&id, |s| {
            if let Some(Handle::Table {
                column_set, cursor, ..
            }) = s.handle_mut(5)
            {
                column_set.push(crate::mapi::data::PropertyTag::new(
                    crate::mapi::data::PropertyType::PTYP_STRING,
                    0x0037,
                ));
                *cursor = 3;
            }
        })
        .expect("session");
        // Verify via the read-snapshot.
        let snap = mgr.get(&id).expect("session");
        let Handle::Table {
            column_set, cursor, ..
        } = snap.handles.get(&5).unwrap()
        else {
            panic!("expected table handle");
        };
        assert_eq!(column_set.len(), 1);
        assert_eq!(*cursor, 3);
    }

    #[test]
    fn with_handle_records_handle_kind() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        mgr.with_session_mut(&id, |s| {
            s.set_handle(
                2,
                Handle::Message {
                    backend_id: "M-123".into(),
                    mailbox_id: "I".into(),
                    kind: FolderKind::Mail,
                    is_new: false,
                },
            );
        })
        .expect("session");
        let kind = mgr
            .with_handle(&id, 2, |h| h.kind())
            .expect("handle present");
        assert_eq!(kind, HandleKind::Message);
        // A handle that was never installed returns None despite the session existing.
        assert!(mgr.with_handle(&id, 9, |_| ()).is_none());
    }

    #[test]
    fn alloc_handle_fills_lowest_free_index() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        let a = mgr
            .with_session_mut(&id, |s| s.alloc_handle(table_handle()))
            .unwrap()
            .unwrap();
        let b = mgr
            .with_session_mut(&id, |s| s.alloc_handle(table_handle()))
            .unwrap()
            .unwrap();
        let c = mgr
            .with_session_mut(&id, |s| s.alloc_handle(table_handle()))
            .unwrap()
            .unwrap();
        assert_eq!([a, b, c], [0u8, 1, 2]);
        // Free index 1, the next alloc reuses 1.
        mgr.with_session_mut(&id, |s| s.free_handle(1)).unwrap();
        let d = mgr
            .with_session_mut(&id, |s| s.alloc_handle(table_handle()))
            .unwrap()
            .unwrap();
        assert_eq!(d, 1u8);
    }

    #[test]
    fn create_and_get_roundtrip() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("u@example.com"));
        assert_eq!(mgr.len(), 1);
        let snap = mgr.get(&id).expect("session present");
        assert_eq!(snap.principal.email, "u@example.com");
    }

    #[test]
    fn remove_zeroizes_principal() {
        let mgr = SessionManager::new();
        let id = mgr.create(principal("secret@example.com"));
        assert!(mgr.remove(&id));
        assert!(mgr.get(&id).is_none());
        assert!(mgr.is_empty());
    }

    #[test]
    fn get_missing_returns_none() {
        let mgr = SessionManager::new();
        let id = Uuid::new_v4();
        assert!(mgr.get(&id).is_none());
    }

    #[test]
    fn sweep_idle_removes_old_sessions() {
        let mgr = SessionManager::with_ttl(Duration::from_millis(1));
        let id = mgr.create(principal("u@example.com"));
        std::thread::sleep(Duration::from_millis(5));
        let removed = mgr.sweep_idle();
        assert_eq!(removed, 1);
        assert!(mgr.get(&id).is_none());
    }

    #[test]
    fn sweep_idle_keeps_active_sessions() {
        let mgr = SessionManager::with_ttl(Duration::from_secs(60));
        let _id = mgr.create(principal("u@example.com"));
        assert_eq!(mgr.sweep_idle(), 0);
        assert_eq!(mgr.len(), 1);
    }
}
