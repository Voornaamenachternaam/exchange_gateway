// src/storage.rs
use sqlx::{Executor, FromRow, Pool, Row, Sqlite};
use std::fmt;
use std::sync::Arc;

use crate::error::{GatewayError, Result};

pub type SqlPool = Pool<Sqlite>;

// Custom Debug trait for safe, production-safe debugging of row structs.
// This avoids accidentally logging sensitive data like content_base64, imei, phone_number.
// Uses fmt::Result pattern for zero-allocation formatting (like std::fmt::Debug).
pub trait SafeDebug {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result;
}

#[derive(Clone)]
pub struct Storage {
    pool: Arc<SqlPool>,
}

impl Storage {
    pub fn pool(&self) -> &SqlPool {
        &self.pool
    }
}

// Row struct for change journal queries - safe for logging (no sensitive data)
#[derive(FromRow)]
pub struct JournalRow {
    pub id: i64,
    pub server_id: String,
    pub op: String,
    pub resource_href: Option<String>,
}

impl SafeDebug for JournalRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JournalRow")
            .field("id", &self.id)
            .field("server_id", &self.server_id)
            .field("op", &self.op)
            .field("resource_href", &self.resource_href)
            .finish()
    }
}

// Row struct for item_map queries - safe for logging (no sensitive data)
#[derive(FromRow)]
pub struct EwsItemRow {
    pub server_id: String,
    pub caldav_href: Option<String>,
    pub resource_href: String,
    pub uid: Option<String>,
    pub etag: Option<String>,
    pub updated_at: Option<String>,
}

impl SafeDebug for EwsItemRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EwsItemRow")
            .field("server_id", &self.server_id)
            .field("resource_href", &self.resource_href)
            .field("caldav_href", &self.caldav_href)
            .field("uid", &self.uid)
            .field("etag", &self.etag)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

// Row struct for contact_map queries
#[derive(FromRow)]
pub struct ContactRow {
    pub id: i64,
    pub owner: String,
    pub carddav_href: String,
    pub server_id: String,
    pub etag: Option<String>,
    pub vcard: Option<String>,
    pub updated_at: Option<String>,
}

impl SafeDebug for ContactRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContactRow")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .field("carddav_href", &self.carddav_href)
            .field("server_id", &self.server_id)
            .field("etag", &self.etag)
            .field("vcard", &self.vcard.as_ref().map(|_| "<redacted>"))
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

// Row struct for task_map queries - safe for logging (body may contain note text)
#[derive(FromRow, Clone)]
pub struct TaskRow {
    pub id: i64,
    pub owner: String,
    pub server_id: String,
    pub subject: Option<String>,
    pub importance: Option<i64>,
    pub sensitivity: Option<i64>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub utc_start_date: Option<String>,
    pub utc_due_date: Option<String>,
    pub complete: i64,
    pub date_completed: Option<String>,
    pub reminder_set: i64,
    pub reminder_time: Option<String>,
    pub categories: Option<String>,
    pub body: Option<String>,
    pub caldav_href: Option<String>,
    pub etag: Option<String>,
    pub uid: Option<String>,
    pub updated_at: Option<String>,
}

impl SafeDebug for TaskRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TaskRow")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .field("server_id", &self.server_id)
            .field("subject", &self.subject.as_ref().map(|_| "<redacted>"))
            .field("complete", &self.complete)
            .field("reminder_set", &self.reminder_set)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

// Row struct for note_map queries - safe for logging (body may contain note text)
#[derive(FromRow, Clone)]
pub struct NoteRow {
    pub id: i64,
    pub owner: String,
    pub server_id: String,
    pub subject: Option<String>,
    pub message_class: Option<String>,
    pub body: Option<String>,
    pub categories: Option<String>,
    pub last_modified_date: Option<String>,
    pub caldav_href: Option<String>,
    pub etag: Option<String>,
    pub uid: Option<String>,
    pub updated_at: Option<String>,
}

impl SafeDebug for NoteRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NoteRow")
            .field("id", &self.id)
            .field("owner", &self.owner)
            .field("server_id", &self.server_id)
            .field("subject", &self.subject.as_ref().map(|_| "<redacted>"))
            .field("message_class", &self.message_class)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}
/// A gateway-local user photo (MS-OXWSPHOTO). The binary blob is logged only as
/// its length (via `SafeDebug`), never as its contents.
#[derive(FromRow)]
pub struct UserPhotoRow {
    pub owner: String,
    pub data: Vec<u8>,
    pub content_type: String,
    pub updated_at: Option<String>,
}

impl SafeDebug for UserPhotoRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UserPhotoRow")
            .field("owner", &self.owner)
            .field("data_len", &self.data.len())
            .field("content_type", &self.content_type)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

pub struct DeviceInfoParams<'a> {
    pub owner: &'a str,
    pub device_id: &'a str,
    pub friendly_name: &'a str,
    pub model: &'a str,
    pub os: &'a str,
    pub phone_number: &'a str,
    pub imei: &'a str,
    pub user_agent: &'a str,
}

pub struct MeetingStateParams<'a> {
    pub owner: &'a str,
    pub uid: &'a str,
    pub sequence: u32,
    pub state: &'a str,
    pub state_flags: u8,
    pub is_organizer: bool,
    pub organizer_email: Option<&'a str>,
    pub organizer_name: Option<&'a str>,
    pub subject: Option<&'a str>,
    pub location: Option<&'a str>,
    pub start_time: &'a str,
    pub end_time: &'a str,
    pub timezone: Option<&'a str>,
}

pub struct MeetingAttendeeParams<'a> {
    pub owner: &'a str,
    pub meeting_uid: &'a str,
    pub email: &'a str,
    pub name: Option<&'a str>,
    pub status: u8,
    pub role: u8,
    pub response_time: Option<&'a str>,
    pub proposed_start: Option<&'a str>,
    pub proposed_end: Option<&'a str>,
    pub sequence: u32,
}

// Row struct for calendar_exceptions queries - safe for logging (no sensitive data)
#[derive(FromRow)]
pub struct CalendarExceptionRow {
    pub parent_server_id: String,
    pub exception_start: String,
    pub server_id: Option<String>,
    pub is_deleted: i32,
    pub created_at: String,
}

impl SafeDebug for CalendarExceptionRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CalendarExceptionRow")
            .field("parent_server_id", &self.parent_server_id)
            .field("exception_start", &self.exception_start)
            .field("server_id", &self.server_id)
            .field("is_deleted", &self.is_deleted)
            .field("created_at", &self.created_at)
            .finish()
    }
}

// Row struct for meeting_response queries - safe for logging (no sensitive data)
#[derive(FromRow)]
pub struct MeetingResponseRow {
    pub request_id: String,
    pub calendar_id: String,
    pub user_response: i32,
    pub created_at: String,
}

impl SafeDebug for MeetingResponseRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeetingResponseRow")
            .field("request_id", &self.request_id)
            .field("calendar_id", &self.calendar_id)
            .field("user_response", &self.user_response)
            .field("created_at", &self.created_at)
            .finish()
    }
}

// Row struct for meeting_rsvp queries - safe for logging (no sensitive data)
#[derive(FromRow)]
pub struct MeetingRsvpRow {
    pub uid: String,
    pub instance_key: String,
    pub decision: i32,
    pub sequence: i64,
    pub message_id: Option<String>,
    pub calendar_server_id: Option<String>,
    pub responded_at: String,
}

/// Parameters for recording one delivered iTIP REPLY in the duplicate-RSVP
/// idempotence store ([MS-ASCMD] §2.2.1.11) — the `MeetingStateParams`
/// borrow convention applied to the meeting_rsvp upsert.
#[derive(Debug)]
pub struct MeetingRsvpRecord<'a> {
    pub owner: &'a str,
    pub uid: &'a str,
    /// "" for a whole-series response, the UTC RECURRENCE-ID otherwise.
    pub instance_key: &'a str,
    pub decision: i32,
    /// The REQUEST SEQUENCE the delivered REPLY answered (RFC 5546 §3.2.1.4).
    pub sequence: i64,
    pub message_id: Option<&'a str>,
    pub calendar_server_id: Option<&'a str>,
}

impl SafeDebug for MeetingRsvpRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeetingRsvpRow")
            .field("uid", &self.uid)
            .field("instance_key", &self.instance_key)
            .field("decision", &self.decision)
            .field("sequence", &self.sequence)
            .field("message_id", &self.message_id)
            .field("calendar_server_id", &self.calendar_server_id)
            .field("responded_at", &self.responded_at)
            .finish()
    }
}

// Row struct for meeting_state queries - subject/location/organizer_email redacted as PII
#[derive(FromRow)]
pub struct MeetingStateRow {
    pub uid: String,
    pub owner: String,
    pub sequence: i32,
    pub state: String,
    pub state_flags: i32,
    pub is_organizer: i32,
    pub organizer_email: Option<String>,
    pub organizer_name: Option<String>,
    pub subject: Option<String>,
    pub location: Option<String>,
    pub start_time: String,
    pub end_time: String,
    pub timezone: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub last_sequence_time: Option<String>,
}

// Helper to safely display a PII field.
// - None -> "None"
// - Some("") (empty) -> "None" (semantically equivalent to no data)
// - Some("...") (non-empty) -> "[redacted]" (sensitive PII)
fn safe_display(val: &Option<String>) -> &str {
    match val {
        Some(v) if v.is_empty() => "None",
        Some(_) => "[redacted]",
        None => "None",
    }
}

// Helper to redact a plain String field (always redacted if non-empty)
fn redact_if_present(val: &str) -> &str {
    if val.is_empty() { "None" } else { "[redacted]" }
}

impl SafeDebug for MeetingStateRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeetingStateRow")
            .field("uid", &self.uid)
            .field("owner", &self.owner)
            .field("sequence", &self.sequence)
            .field("state", &self.state)
            .field("state_flags", &self.state_flags)
            .field("is_organizer", &self.is_organizer)
            .field("organizer_email", &safe_display(&self.organizer_email)) // Redacted - PII
            .field("organizer_name", &safe_display(&self.organizer_name)) // Redacted - PII
            .field("subject", &safe_display(&self.subject)) // Redacted - may contain PII/confidential
            .field("location", &safe_display(&self.location)) // Redacted - may contain PII/confidential
            .field("start_time", &self.start_time)
            .field("end_time", &self.end_time)
            .field("timezone", &self.timezone)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("last_sequence_time", &self.last_sequence_time)
            .finish()
    }
}

// Row struct for meeting_attendee queries - email/name redacted as PII
#[derive(FromRow)]
pub struct MeetingAttendeeRow {
    pub meeting_uid: String,
    pub owner: String,
    pub email: String,
    pub name: Option<String>,
    pub status: i32,
    pub role: i32,
    pub response_time: Option<String>,
    pub proposed_start: Option<String>,
    pub proposed_end: Option<String>,
    pub sequence: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl SafeDebug for MeetingAttendeeRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MeetingAttendeeRow")
            .field("meeting_uid", &self.meeting_uid)
            .field("owner", &self.owner)
            .field("email", &redact_if_present(&self.email)) // Redacted - PII
            .field("name", &safe_display(&self.name)) // Redacted - PII
            .field("status", &self.status)
            .field("role", &self.role)
            .field("response_time", &self.response_time)
            .field("proposed_start", &self.proposed_start)
            .field("proposed_end", &self.proposed_end)
            .field("sequence", &self.sequence)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

// Row struct for meeting_scheduling_queue queries - ical_data redacted
#[derive(FromRow)]
pub struct SchedulingQueueRow {
    pub id: i64,
    pub meeting_uid: String,
    pub owner: String,
    pub operation: String,
    pub sequence: i32,
    pub ical_data: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub last_attempt: Option<String>,
    pub error_message: Option<String>,
    pub created_at: String,
    pub processed_at: Option<String>,
}

impl SafeDebug for SchedulingQueueRow {
    fn safe_debug(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SchedulingQueueRow")
            .field("id", &self.id)
            .field("meeting_uid", &self.meeting_uid)
            .field("owner", &self.owner)
            .field("operation", &self.operation)
            .field("sequence", &self.sequence)
            .field("ical_data", &self.ical_data.as_ref().map(|_| "[redacted]")) // Always redacted
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field("last_attempt", &self.last_attempt)
            .field("error_message", &self.error_message)
            .field("created_at", &self.created_at)
            .field("processed_at", &self.processed_at)
            .finish()
    }
}

impl Storage {
    /// Busy timeout for writer contention. WAL mode gives readers a snapshot
    /// that never blocks on the single writer, but concurrent writes still
    /// serialize on the writer lock; without a busy timeout the loser of the
    /// race fails immediately with SQLITE_BUSY instead of waiting, which is
    /// what turns concurrent EAS long-poll sync-state writes into ping
    /// latency spikes. 5s is far above the sub-millisecond duration of the
    /// journal/mirror writes this gateway performs.
    const BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

    pub async fn new(database_url: &str) -> Result<Self> {
        use sqlx::sqlite::{
            SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous,
        };
        use std::str::FromStr;
        let options = SqliteConnectOptions::from_str(database_url)
            .map_err(|e| {
                tracing::error!("Invalid SQLite URL: {}", e);
                GatewayError::Storage(format!("Invalid SQLite URL: {}", e))
            })?
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Self::BUSY_TIMEOUT)
            .synchronous(SqliteSynchronous::Normal) // safe under WAL; full fsync per commit is unnecessary (durability comes from the WAL checkpoints)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            // One writer at a time is all SQLite grants regardless of pool
            // size; extra connections only buy parallelism for WAL readers.
            // Match the sqlx default of 10 rather than inflating the pool.
            .connect_with(options)
            .await
            .map_err(|e| {
                tracing::error!("Failed to create SQLite pool: {}", e);
                GatewayError::Storage(e.to_string())
            })?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    pub async fn init_schema(&self) -> Result<()> {
        let schema = include_str!("../sqlite_schema.sql");
        self.pool.execute(schema).await.map_err(|e| {
            tracing::error!("Schema init error: {}", e);
            GatewayError::Storage(format!("Schema init error: {}", e))
        })?;
        // Idempotent column migration: sqlite_schema.sql only creates tables
        // when absent, so databases created before `journal_seq` existed need
        // an explicit ALTER. Probe the column list rather than suppressing the
        // "duplicate column" error.
        let columns = sqlx::query("PRAGMA table_info(sync_state)")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let has_journal_seq = columns
            .iter()
            .any(|c| c.get::<String, _>("name") == "journal_seq");
        if !has_journal_seq {
            sqlx::query("ALTER TABLE sync_state ADD COLUMN journal_seq INTEGER")
                .execute(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Migration error: {}", e)))?;
        }
        // Same probe for the `base_seq` column of `local_sync_cursor`: it
        // anchors continuation windows to the journal head captured when
        // the initial sync started (see sqlite_schema.sql).
        let cursor_columns = sqlx::query("PRAGMA table_info(local_sync_cursor)")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let has_base_seq = cursor_columns
            .iter()
            .any(|c| c.get::<String, _>("name") == "base_seq");
        if !has_base_seq {
            sqlx::query(
                "ALTER TABLE local_sync_cursor ADD COLUMN base_seq INTEGER NOT NULL DEFAULT 0",
            )
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Migration error: {}", e)))?;
        }
        // Same probe for the `sequence` column of `meeting_rsvp`: databases
        // created between the v9 table's introduction and the SEQUENCE-aware
        // idempotence key need the column added — the recorded REPLY answers a
        // specific REQUEST revision (RFC 5546 §3.2.1.4).
        let rsvp_columns = sqlx::query("PRAGMA table_info(meeting_rsvp)")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let has_sequence = rsvp_columns
            .iter()
            .any(|c| c.get::<String, _>("name") == "sequence");
        if !has_sequence {
            sqlx::query("ALTER TABLE meeting_rsvp ADD COLUMN sequence INTEGER NOT NULL DEFAULT 0")
                .execute(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Migration error: {}", e)))?;
        }
        // Same probe for the backend-sync columns of `task_map` and `note_map`:
        // databases created before Tasks/Notes were synced to Stalwart CalDAV
        // (VTODO/VJOURNAL) carry rows without a backend identity; the columns
        // let the first reconcile push them into the backend and afterwards
        // track the authoritative resource (href + ETag + UID).
        for table in ["task_map", "note_map"] {
            let columns = sqlx::query(&format!("PRAGMA table_info({table})"))
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
            let names: Vec<String> = columns.iter().map(|c| c.get::<String, _>("name")).collect();
            for (column, ddl) in [
                (
                    "caldav_href",
                    format!("ALTER TABLE {table} ADD COLUMN caldav_href TEXT"),
                ),
                ("etag", format!("ALTER TABLE {table} ADD COLUMN etag TEXT")),
                ("uid", format!("ALTER TABLE {table} ADD COLUMN uid TEXT")),
            ] {
                if !names.iter().any(|n| n == column) {
                    sqlx::query(&ddl)
                        .execute(self.pool.as_ref())
                        .await
                        .map_err(|e| GatewayError::Storage(format!("Migration error: {}", e)))?;
                }
            }
        }
        // The href/uid indexes are created HERE, not in sqlite_schema.sql:
        // `CREATE INDEX` resolves its columns at execution, so on a pre-§15
        // database (whose task_map/note_map lacked caldav_href/uid) the schema
        // script would abort with "no such column" before the ALTERs above
        // could add them. After the migration the columns always exist, so
        // IF NOT EXISTS keeps this idempotent for fresh and migrated databases.
        for (index, table) in [
            ("idx_task_map_href", "task_map"),
            ("idx_task_map_uid", "task_map"),
            ("idx_note_map_href", "note_map"),
            ("idx_note_map_uid", "note_map"),
        ] {
            let column = if index.ends_with("_href") {
                "caldav_href"
            } else {
                "uid"
            };
            sqlx::query(&format!(
                "CREATE INDEX IF NOT EXISTS {index} ON {table}(owner, {column})"
            ))
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Migration error: {}", e)))?;
        }
        Ok(())
    }

    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO sync_state (owner, collection_id, sync_key, token) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, collection_id) DO UPDATE SET sync_key = ?3, token = ?4, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(collection_id)
        .bind(sync_key)
        .bind(token)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        let row = sqlx::query(
            "SELECT sync_key, token FROM sync_state WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    /// Persist the sticky Sync `<Options>` block for one device-scoped
    /// collection ([MS-ASCMD] §2.2.3.125.6). The stored form is the resolved
    /// negotiation state as JSON; it is replaced wholesale whenever a
    /// request carries a new `<Options>` block and reused verbatim when a
    /// request carries none.
    pub async fn set_sync_collection_options(
        &self,
        owner: &str,
        collection_id: &str,
        options: &crate::eas_sync_options::EasSyncCollectionOptions,
    ) -> Result<()> {
        let json = serde_json::to_string(options)
            .map_err(|e| GatewayError::Storage(format!("Options serialize error: {}", e)))?;
        sqlx::query(
            "INSERT INTO sync_collection_options (owner, collection_id, options_json)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(owner, collection_id) DO UPDATE
             SET options_json = ?3, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(&json)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Read the sticky Sync `<Options>` block for one device-scoped
    /// collection, if a previous request established one.
    pub async fn get_sync_collection_options(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<crate::eas_sync_options::EasSyncCollectionOptions>> {
        let row = sqlx::query(
            "SELECT options_json FROM sync_collection_options WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let Some(row) = row else { return Ok(None) };
        let json: String = row.get(0);
        let options = serde_json::from_str(&json)
            .map_err(|e| GatewayError::Storage(format!("Options deserialize error: {}", e)))?;
        Ok(Some(options))
    }

    pub async fn upsert_item_map(
        &self,
        owner: &str,
        caldav_href: &str,
        resource_href: &str,
        server_id: &str,
        uid: &str,
        etag: &str,
    ) -> Result<()> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        sqlx::query(
            "INSERT INTO item_map (owner, caldav_href, resource_href, server_id, uid, etag) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(owner, server_id) DO UPDATE SET caldav_href = ?2, resource_href = ?3, uid = ?5, etag = ?6, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(caldav_href)
        .bind(resource_href)
        .bind(server_id)
        .bind(uid)
        .bind(etag)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        sqlx::query(
            "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'upsert', ?3)"
        )
        .bind(owner)
        .bind(server_id)
        .bind(resource_href)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(())
    }

    pub async fn get_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        let row = sqlx::query(
            "SELECT server_id, status FROM client_sync_command WHERE owner = ?1 AND collection_id = ?2 AND client_id = ?3"
        )
        .bind(owner)
        .bind(collection_id)
        .bind(client_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    pub async fn put_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
        server_id: Option<&str>,
        status: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO client_sync_command (owner, collection_id, client_id, server_id, status) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(owner, collection_id, client_id) DO UPDATE SET server_id = ?4, status = ?5, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(collection_id)
        .bind(client_id)
        .bind(server_id)
        .bind(status)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM item_map WHERE owner = ?1 AND server_id = ?2")
            .bind(owner)
            .bind(server_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        sqlx::query(
            "INSERT OR REPLACE INTO deleted_item_tombstone (owner, server_id) VALUES (?1, ?2)",
        )
        .bind(owner)
        .bind(server_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        sqlx::query("INSERT INTO change_journal (owner, server_id, op) VALUES (?1, ?2, 'delete')")
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(())
    }

    pub async fn list_changes_since(
        &self,
        owner: &str,
        collection_id: &str,
        since_timestamp: &str,
        limit: i64,
    ) -> Result<Vec<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT im.server_id, im.caldav_href, im.resource_href, im.uid, im.etag, im.updated_at
             FROM item_map im
             WHERE im.owner = ?1 AND im.resource_href LIKE ?2 || '%'
             AND im.updated_at > ?3
             ORDER BY im.updated_at ASC, im.server_id ASC
             LIMIT ?4",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(since_timestamp)
        .bind(limit)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn list_deleted_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT server_id FROM deleted_item_tombstone WHERE owner = ?1 AND deleted_at > datetime(?2, 'unixepoch')")
            .bind(owner)
            .bind(since_unix_ts)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(id), 0) FROM change_journal")
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.get(0))
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since: i64,
        limit: i64,
    ) -> Result<Vec<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT im.server_id, im.caldav_href, im.resource_href, im.uid, im.etag, im.updated_at
             FROM item_map im
             WHERE im.owner = ?1 AND im.server_id IN (
                 SELECT server_id FROM change_journal WHERE owner = ?1 AND id > ?2 AND op = 'upsert'
             )
             ORDER BY im.server_id ASC
             LIMIT ?3",
        )
        .bind(owner)
        .bind(since)
        .bind(limit)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn list_deleted_since_seq(
        &self,
        owner: &str,
        since: i64,
    ) -> Result<Vec<(i64, String)>> {
        // Exclude gateway-local Tasks/Notes tombstones (resource_href 'task'/'note')
        // so item_map-backed content classes (Calendar/Email/Contacts) never treat
        // a task/note deletion as one of their own. Legacy deletes carry a NULL
        // resource_href and are still included.
        let rows = sqlx::query(
            "SELECT id, server_id FROM change_journal WHERE owner = ?1 AND id > ?2 AND op = 'delete' AND (resource_href IS NULL OR resource_href NOT IN ('task', 'note')) ORDER BY id ASC"
        )
            .bind(owner)
            .bind(since)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows.into_iter().map(|r| (r.get(0), r.get(1))).collect())
    }

    pub async fn list_journal_since_seq(&self, owner: &str, since: i64) -> Result<Vec<JournalRow>> {
        sqlx::query_as::<_, JournalRow>(
            "SELECT id, server_id, op, resource_href FROM change_journal WHERE owner = ?1 AND id > ?2 ORDER BY id ASC"
        )
        .bind(owner)
        .bind(since)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    /// Live item_map rows in stable `server_id` ASC order, strictly after
    /// `after_server_id` (keyset pagination over the live snapshot).
    ///
    /// Used to re-prime a client whose journal window was pruned: the journal
    /// below the floor is gone, so the live set must be enumerated directly
    /// from `item_map` rather than reconstructed from journal rows. `limit`
    /// bounds the fetch so a page does not load the whole set.
    pub async fn list_changes_after_server_id(
        &self,
        owner: &str,
        after_server_id: Option<&str>,
        limit: u64,
    ) -> Result<Vec<EwsItemRow>> {
        match after_server_id {
            Some(after) => sqlx::query_as::<_, EwsItemRow>(
                "SELECT server_id, caldav_href, resource_href, uid, etag, updated_at \
                     FROM item_map WHERE owner = ?1 AND server_id > ?2 \
                     ORDER BY server_id ASC LIMIT ?3",
            )
            .bind(owner)
            .bind(after)
            .bind(limit as i64)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e))),
            None => sqlx::query_as::<_, EwsItemRow>(
                "SELECT server_id, caldav_href, resource_href, uid, etag, updated_at \
                     FROM item_map WHERE owner = ?1 \
                     ORDER BY server_id ASC LIMIT ?2",
            )
            .bind(owner)
            .bind(limit as i64)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e))),
        }
    }

    /// Lowest journal watermark any device of `owner` could still ask from.
    /// Both watermark stores count: `token` values shaped `seq:<n>` and the
    /// `journal_seq` column populated by non-email sync completions (their
    /// `token` may carry a provider state such as a JMAP `query_state`).
    /// `None` when no watermark exists, in which case nothing may be pruned
    /// for this owner.
    async fn min_seq_watermark(&self, owner: &str) -> Result<Option<i64>> {
        let row = sqlx::query(
            "SELECT MIN(w) FROM ( \
                 SELECT CAST(substr(token, 5) AS INTEGER) AS w \
                 FROM sync_state \
                 WHERE owner = ?1 AND substr(token, 1, 4) = 'seq:' \
                 AND substr(token, 5) GLOB '[0-9]*' AND substr(token, 5) != '' \
                 UNION ALL \
                 SELECT journal_seq FROM sync_state \
                 WHERE owner = ?1 AND journal_seq IS NOT NULL \
             )",
        )
        .bind(owner)
        .fetch_one(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.get(0))
    }

    /// The journal watermark a Ping / GetItemEstimate may rely on for a
    /// collection: the higher of the `seq:` token and the `journal_seq`
    /// column, both of which are refreshed on every sync completion.
    pub async fn journal_watermark(&self, owner: &str, collection_id: &str) -> Result<Option<i64>> {
        let row = sqlx::query(
            "SELECT token, journal_seq FROM sync_state WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let Some(row) = row else { return Ok(None) };
        let token_seq: Option<i64> = row
            .get::<Option<String>, _>(0)
            .as_deref()
            .and_then(|t| t.strip_prefix("seq:"))
            .and_then(|v| v.parse::<i64>().ok());
        let column_seq: Option<i64> = row.get(1);
        Ok(token_seq.into_iter().chain(column_seq).max())
    }

    /// Store `seq` as the journal watermark for a collection. Semantics: all
    /// change-journal rows with `id <= seq` were already delivered to the
    /// client by the sync that just completed; a later Ping or estimate must
    /// only look at rows above it.
    pub async fn set_journal_watermark(
        &self,
        owner: &str,
        collection_id: &str,
        seq: i64,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE sync_state SET journal_seq = ?3 WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(seq)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Record the current journal head as the collection's watermark — the
    /// sync just delivered everything journaled so far.
    pub async fn record_journal_watermark(&self, owner: &str, collection_id: &str) -> Result<()> {
        let head = self.get_latest_change_seq().await?;
        self.set_journal_watermark(owner, collection_id, head).await
    }

    /// The highest change-journal id whose rows have been pruned for `owner`.
    ///
    /// `0` means nothing has been pruned yet (every journal id is still
    /// replayable). Anything at or below the returned value is gone for good.
    pub async fn pruned_through(&self, owner: &str) -> Result<i64> {
        let row = sqlx::query("SELECT pruned_through FROM journal_prune_state WHERE owner = ?1")
            .bind(owner)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.map(|r| r.get::<i64, _>(0)).unwrap_or(0))
    }

    /// Whether a sync resuming from journal position `since` can no longer
    /// reconstruct its delta because pruning already removed part of the
    /// window it needs.
    ///
    /// A device resuming from `since` requires every row with `id > since`;
    /// pruning removed every row with `id <= pruned_through`. The window is
    /// therefore intact exactly when `since >= pruned_through`. A stale
    /// watermark (a device offline longer than the pruning safety window, or
    /// one whose watermark the floor scan could not observe) must be told to
    /// re-prime rather than silently resume across a re-keyed gap.
    pub async fn journal_window_stale(&self, owner: &str, since: i64) -> Result<bool> {
        Ok(since < self.pruned_through(owner).await?)
    }

    /// Delete change-journal rows no device of `owner` can ever request again.
    ///
    /// Every `id > watermark` lookup a device will ever issue starts from a
    /// `seq:` watermark stored in `sync_state`; rows at or below the *minimum*
    /// such watermark are therefore permanently invisible to sync and Ping,
    /// and removing them keeps the journal bounded without ever cutting a
    /// delta-sync window out from under a device. Rows are kept whenever no
    /// seq watermark exists for the owner (nothing synced yet — full replay
    /// must remain possible after a container restart).
    ///
    /// The deleted floor is recorded in `journal_prune_state` so a later sync
    /// resuming below it is detected as stale and forced to re-prime instead
    /// of silently missing the pruned delta (`journal_window_stale`).
    pub async fn prune_change_journal(&self, owner: &str) -> Result<u64> {
        let Some(floor) = self.min_seq_watermark(owner).await? else {
            return Ok(0);
        };
        // The DELETE and the floor upsert MUST be one atomic unit: if the
        // delete committed but the floor upsert did not (process stop, storage
        // error, or a concurrent sync reading between them), `pruned_through`
        // would still report the old floor while the rows at/below the new one
        // are gone — stale resumes would then silently miss changes. Commit
        // only after the floor is recorded (SQLite is single-writer, so a
        // read outside the transaction sees either both or neither).
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;
        let res = sqlx::query("DELETE FROM change_journal WHERE owner = ?1 AND id <= ?2")
            .bind(owner)
            .bind(floor)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("Delete error: {}", e)))?;
        if floor > 0 {
            // Advance the recorded floor monotonically: a later, lower floor
            // (e.g. a fresh device syncing from 0) must not erase the fact
            // that rows at the old floor were already removed.
            sqlx::query(
                "INSERT INTO journal_prune_state (owner, pruned_through) VALUES (?1, ?2) \
                 ON CONFLICT(owner) DO UPDATE SET \
                 pruned_through = MAX(pruned_through, ?2), updated_at = CURRENT_TIMESTAMP",
            )
            .bind(owner)
            .bind(floor)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        }
        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(res.rows_affected())
    }

    /// Watermark-safe pruning for every owner present in the journal.
    pub async fn prune_change_journals(&self) -> Result<u64> {
        let rows = sqlx::query("SELECT DISTINCT owner FROM change_journal")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        let mut total = 0u64;
        for row in rows {
            let owner: String = row.get(0);
            total += self.prune_change_journal(&owner).await?;
        }
        Ok(total)
    }

    pub async fn set_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
        policy_key: &str,
        policy_status: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO provision_state (owner, device_id, policy_key, policy_status) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, device_id) DO UPDATE SET policy_key = ?3, policy_status = ?4, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(device_id)
        .bind(policy_key)
        .bind(policy_status)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<(String, String)>> {
        let row = sqlx::query(
            "SELECT policy_key, policy_status FROM provision_state WHERE owner = ?1 AND device_id = ?2"
        )
        .bind(owner)
        .bind(device_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    pub async fn upsert_device_info(&self, params: &DeviceInfoParams<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO device_info (user_email, device_id, friendly_name, model, os, phone_number, imei, user_agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(user_email, device_id) DO UPDATE SET
             friendly_name = ?3, model = ?4, os = ?5, phone_number = ?6, imei = ?7, user_agent = ?8, last_seen = CURRENT_TIMESTAMP"
        )
        .bind(params.owner)
        .bind(params.device_id)
        .bind(params.friendly_name)
        .bind(params.model)
        .bind(params.os)
        .bind(params.phone_number)
        .bind(params.imei)
        .bind(params.user_agent)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn list_ews_items(
        &self,
        owner: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT server_id, caldav_href, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 ORDER BY updated_at DESC, server_id ASC LIMIT ?2 OFFSET ?3"
        )
        .bind(owner)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    /// Persist the EAS email initial-sync paging cursor
    /// (`position`, captured JMAP state token) for a collection.
    ///
    /// Written by the email Sync path (MS-ASCMD §2.2.3.199) when a window
    /// fills before the mailbox is drained; the next Sync resumes at
    /// `position`. An empty `state_token` stores NULL.
    pub async fn set_email_sync_cursor(
        &self,
        owner: &str,
        collection_id: &str,
        position: u64,
        state_token: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO email_sync_cursor (owner, collection_id, position, state_token)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, collection_id) DO UPDATE
             SET position = ?3, state_token = ?4, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(position as i64)
        .bind(state_token)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Read the EAS email initial-sync paging cursor for a collection, if a
    /// partially-delivered initial sync left one behind.
    pub async fn get_email_sync_cursor(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(u64, Option<String>)>> {
        let row = sqlx::query(
            "SELECT position, state_token FROM email_sync_cursor
             WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.map(|r| {
            let position: i64 = r.get(0);
            (position.max(0) as u64, r.get(1))
        }))
    }

    /// Drop the EAS email initial-sync paging cursor once the mailbox has
    /// been fully drained (or when a fresh initial sync restarts from zero).
    pub async fn clear_email_sync_cursor(&self, owner: &str, collection_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM email_sync_cursor WHERE owner = ?1 AND collection_id = ?2")
            .bind(owner)
            .bind(collection_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Persist the EAS email delta-sync overflow queue for a collection.
    ///
    /// `ops_json` is a JSON array of `{"op":"add|change|delete","id":"…"}`
    /// entries; `base_state` is the JMAP Email state token that follows the
    /// full accumulated change set (see `email_sync_pending` in
    /// `sqlite_schema.sql`).
    pub async fn set_email_sync_pending(
        &self,
        owner: &str,
        collection_id: &str,
        ops_json: &str,
        base_state: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO email_sync_pending (owner, collection_id, ops_json, base_state)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, collection_id) DO UPDATE
             SET ops_json = ?3, base_state = ?4, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(ops_json)
        .bind(base_state)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Read the EAS email delta-sync overflow queue for a collection.
    /// Returns `None` when no overflow is pending.
    pub async fn get_email_sync_pending(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, String)>> {
        let row = sqlx::query(
            "SELECT ops_json, base_state FROM email_sync_pending
             WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    /// Drop the EAS email delta-sync overflow queue (queue fully drained).
    pub async fn clear_email_sync_pending(&self, owner: &str, collection_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM email_sync_pending WHERE owner = ?1 AND collection_id = ?2")
            .bind(owner)
            .bind(collection_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Persist the gateway-local (Tasks/Notes) initial-sync keyset cursor:
    /// `last_key` is the highest server_id already delivered in server_id
    /// ASC order, and `base_seq` is the change-journal head captured when
    /// the SyncKey "0" window started (MS-ASCMD §2.2.3.199 windowing).
    ///
    /// `base_seq` anchors the journal watermark of every continuation
    /// window: mutations recorded while the initial sync paginates stay
    /// above it and are re-delivered by the first delta after the cursor
    /// drains, instead of being consumed (and pruned) unseen.
    pub async fn set_local_sync_cursor(
        &self,
        owner: &str,
        collection_id: &str,
        last_key: &str,
        base_seq: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO local_sync_cursor (owner, collection_id, last_key, base_seq)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, collection_id) DO UPDATE
             SET last_key = ?3, base_seq = ?4, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(last_key)
        .bind(base_seq)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Read the gateway-local (Tasks/Notes) initial-sync keyset cursor for a
    /// collection, if a windowed initial sync is still in progress.
    /// Returns `(last_key, base_seq)`.
    pub async fn get_local_sync_cursor(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, i64)>> {
        let row = sqlx::query(
            "SELECT last_key, base_seq FROM local_sync_cursor WHERE owner = ?1 AND collection_id = ?2",
        )
        .bind(owner)
        .bind(collection_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.map(|r| (r.get(0), r.get(1))))
    }

    /// Drop the gateway-local (Tasks/Notes) initial-sync keyset cursor IFF it
    /// still holds `expected_last_key` and `expected_base_seq`.
    ///
    /// The stale-watermark guard clears a cursor to abandon an in-flight
    /// windowed prime, but the guard reads the cursor before deciding and the
    /// clear happens after — a concurrent `SyncKey 0` prime can persist a
    /// fresh cursor in between. Deleting unconditionally would then erase that
    /// new cursor, and the prime's `MoreAvailable` continuation would fall into
    /// delta sync and skip the items not yet sent. Matching the observed
    /// values makes the clear a compare-and-set: a changed cursor (a different
    /// prime or a new window position) is left intact.
    pub async fn clear_local_sync_cursor_if(
        &self,
        owner: &str,
        collection_id: &str,
        expected_last_key: &str,
        expected_base_seq: i64,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM local_sync_cursor WHERE owner = ?1 AND collection_id = ?2 \
             AND last_key = ?3 AND base_seq = ?4",
        )
        .bind(owner)
        .bind(collection_id)
        .bind(expected_last_key)
        .bind(expected_base_seq)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Drop the gateway-local (Tasks/Notes) initial-sync keyset cursor once
    /// the live set has been fully delivered.
    pub async fn clear_local_sync_cursor(&self, owner: &str, collection_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM local_sync_cursor WHERE owner = ?1 AND collection_id = ?2")
            .bind(owner)
            .bind(collection_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Live Tasks (`resource_href = 'task'`) in stable server_id ASC order,
    /// strictly after `after_server_id` (keyset pagination for windowed
    /// initial sync, MS-ASCMD §2.2.3.199). `None` delivers from the start.
    ///
    /// `limit` bounds the fetch so a continuation window never loads the
    /// whole remaining set; the caller asks for `window + 1` rows and uses
    /// the extra row only to decide `<MoreAvailable/>`.
    pub async fn list_tasks_after(
        &self,
        owner: &str,
        after_server_id: Option<&str>,
        limit: u64,
    ) -> Result<Vec<TaskRow>> {
        match after_server_id {
            Some(after) => {
                sqlx::query_as::<_, TaskRow>(
                    "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, \
                     utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, \
                     categories, body, caldav_href, etag, uid, updated_at FROM task_map \
                     WHERE owner = ?1 AND server_id > ?2 ORDER BY server_id ASC LIMIT ?3",
                )
                .bind(owner)
                .bind(after)
                .bind(limit as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
            }
            None => {
                sqlx::query_as::<_, TaskRow>(
                    "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, \
                     utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, \
                     categories, body, caldav_href, etag, uid, updated_at FROM task_map \
                     WHERE owner = ?1 ORDER BY server_id ASC LIMIT ?2",
                )
                .bind(owner)
                .bind(limit as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
            }
        }
    }

    /// Live Notes (`resource_href = 'note'`) in stable server_id ASC order,
    /// strictly after `after_server_id` (keyset pagination for windowed
    /// initial sync, MS-ASCMD §2.2.3.199). `None` delivers from the start.
    ///
    /// `limit` bounds the fetch so a continuation window never loads the
    /// whole remaining set; the caller asks for `window + 1` rows and uses
    /// the extra row only to decide `<MoreAvailable/>`.
    pub async fn list_notes_after(
        &self,
        owner: &str,
        after_server_id: Option<&str>,
        limit: u64,
    ) -> Result<Vec<NoteRow>> {
        match after_server_id {
            Some(after) => {
                sqlx::query_as::<_, NoteRow>(
                    "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, \
                     caldav_href, etag, uid, updated_at \
                     FROM note_map WHERE owner = ?1 AND server_id > ?2 ORDER BY server_id ASC LIMIT ?3",
                )
                .bind(owner)
                .bind(after)
                .bind(limit as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
            }
            None => {
                sqlx::query_as::<_, NoteRow>(
                    "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, \
                     caldav_href, etag, uid, updated_at \
                     FROM note_map WHERE owner = ?1 ORDER BY server_id ASC LIMIT ?2",
                )
                .bind(owner)
                .bind(limit as i64)
                .fetch_all(self.pool.as_ref())
                .await
                .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
            }
        }
    }

    /// Change-journal rows for one gateway-local content class
    /// (`resource_href = 'task'` or `'note'`) with `id > since`, in journal
    /// id ASC order — the resumable delta source for Tasks/Notes Sync
    /// (MS-ASCMD §2.2.3.199). Rows above the owner's minimum watermark are
    /// never pruned, so resuming from a stored `seq:<id>` token cannot miss
    /// anything.
    pub async fn list_local_content_changes_since_seq(
        &self,
        owner: &str,
        resource_href: &str,
        since: i64,
    ) -> Result<Vec<JournalRow>> {
        sqlx::query_as::<_, JournalRow>(
            "SELECT id, server_id, op, resource_href FROM change_journal \
             WHERE owner = ?1 AND id > ?2 AND resource_href = ?3 ORDER BY id ASC",
        )
        .bind(owner)
        .bind(since)
        .bind(resource_href)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn get_ews_sync_state(&self, owner: &str, folder_id: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT sync_state FROM ews_sync_state WHERE user_email = ?1 AND folder_id = ?2",
        )
        .bind(owner)
        .bind(folder_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(row.map(|r| r.get(0)))
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT server_id, caldav_href, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 AND server_id = ?2"
        )
        .bind(owner)
        .bind(server_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    /// Look up the most recently updated `item_map` row matching a calendar
    /// event's iCal UID.
    ///
    /// The same event is exposed via CalDAV (href-derived `server_id`) and JMAP
    /// (`jmap:`-derived `server_id`). This lookup lets the JMAP read path reuse
    /// the `server_id` already issued under CalDAV, so flipping the calendar
    /// backend does not present existing clients with the same event under a
    /// new Exchange ID.
    pub async fn get_ews_item_by_uid(&self, owner: &str, uid: &str) -> Result<Option<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT server_id, caldav_href, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 AND uid = ?2 ORDER BY updated_at DESC, server_id ASC LIMIT 1"
        )
        .bind(owner)
        .bind(uid)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn verify_item_owner(&self, owner: &str, server_id: &str) -> Result<bool> {
        let row = sqlx::query("SELECT 1 FROM item_map WHERE owner = ?1 AND server_id = ?2 LIMIT 1")
            .bind(owner)
            .bind(server_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.is_some())
    }

    pub async fn get_item_owner(&self, server_id: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT owner FROM item_map WHERE server_id = ?1")
            .bind(server_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row.map(|r| r.get(0)))
    }

    pub async fn set_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
        sync_state: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO ews_sync_state (user_email, folder_id, sync_state) VALUES (?1, ?2, ?3)
             ON CONFLICT(user_email, folder_id) DO UPDATE SET sync_state = ?3",
        )
        .bind(owner)
        .bind(folder_id)
        .bind(sync_state)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn upsert_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
        server_id: Option<&str>,
        is_deleted: bool,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO calendar_exceptions (owner, parent_server_id, exception_start, server_id, is_deleted) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(owner, parent_server_id, exception_start) DO UPDATE SET server_id = ?4, is_deleted = ?5"
        )
        .bind(owner)
        .bind(parent_server_id)
        .bind(exception_start)
        .bind(server_id)
        .bind(is_deleted as i32)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_calendar_exceptions(
        &self,
        owner: &str,
        parent_server_id: &str,
    ) -> Result<Vec<CalendarExceptionRow>> {
        sqlx::query_as::<_, CalendarExceptionRow>(
            "SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2"
        )
        .bind(owner)
        .bind(parent_server_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn get_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<Option<CalendarExceptionRow>> {
        sqlx::query_as::<_, CalendarExceptionRow>(
            "SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2 AND exception_start = ?3"
        )
        .bind(owner)
        .bind(parent_server_id)
        .bind(exception_start)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn delete_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<()> {
        sqlx::query("DELETE FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2 AND exception_start = ?3")
            .bind(owner)
            .bind(parent_server_id)
            .bind(exception_start)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn record_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
        calendar_id: &str,
        user_response: i32,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO meeting_response (owner, request_id, calendar_id, user_response) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(owner, request_id) DO UPDATE SET calendar_id = ?3, user_response = ?4"
        )
        .bind(owner)
        .bind(request_id)
        .bind(calendar_id)
        .bind(user_response)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<MeetingResponseRow>> {
        sqlx::query_as::<_, MeetingResponseRow>(
            "SELECT request_id, calendar_id, user_response, created_at FROM meeting_response WHERE owner = ?1 AND request_id = ?2"
        )
        .bind(owner)
        .bind(request_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    /// Record the attendee's delivered iTIP response for one (uid, instance).
    ///
    /// This is the duplicate-RSVP idempotence store: `instance_key` is empty
    /// for a whole-series response and the UTC RECURRENCE-ID string for a
    /// single-instance response. See `sqlite_schema.sql` (`meeting_rsvp`).
    pub async fn upsert_meeting_rsvp(&self, record: &MeetingRsvpRecord<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO meeting_rsvp (owner, uid, instance_key, decision, sequence, message_id, calendar_server_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(owner, uid, instance_key) DO UPDATE SET
                decision = ?4,
                sequence = ?5,
                message_id = ?6,
                calendar_server_id = ?7,
                responded_at = CURRENT_TIMESTAMP",
        )
        .bind(record.owner)
        .bind(record.uid)
        .bind(record.instance_key)
        .bind(record.decision)
        .bind(record.sequence)
        .bind(record.message_id)
        .bind(record.calendar_server_id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Read the attendee's last delivered iTIP response for one (uid,
    /// instance), if any.
    pub async fn get_meeting_rsvp(
        &self,
        owner: &str,
        uid: &str,
        instance_key: &str,
    ) -> Result<Option<MeetingRsvpRow>> {
        sqlx::query_as::<_, MeetingRsvpRow>(
            "SELECT uid, instance_key, decision, sequence, message_id, calendar_server_id, responded_at
             FROM meeting_rsvp WHERE owner = ?1 AND uid = ?2 AND instance_key = ?3",
        )
        .bind(owner)
        .bind(uid)
        .bind(instance_key)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn upsert_meeting_state(&self, params: &MeetingStateParams<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO meeting_state (uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(owner, uid) DO UPDATE SET
             sequence = ?3, state = ?4, state_flags = ?5, is_organizer = ?6, organizer_email = ?7, organizer_name = ?8,
             subject = ?9, location = ?10, start_time = ?11, end_time = ?12, timezone = ?13,
             last_sequence_time = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(params.uid)
        .bind(params.owner)
        .bind(params.sequence)
        .bind(params.state)
        .bind(params.state_flags)
        .bind(params.is_organizer as i32)
        .bind(params.organizer_email)
        .bind(params.organizer_name)
        .bind(params.subject)
        .bind(params.location)
        .bind(params.start_time)
        .bind(params.end_time)
        .bind(params.timezone)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_meeting_state(
        &self,
        owner: &str,
        uid: &str,
    ) -> Result<Option<MeetingStateRow>> {
        sqlx::query_as::<_, MeetingStateRow>(
            "SELECT uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone, created_at, updated_at, last_sequence_time
             FROM meeting_state WHERE owner = ?1 AND uid = ?2"
        )
        .bind(owner)
        .bind(uid)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn delete_meeting_state(&self, owner: &str, uid: &str) -> Result<()> {
        sqlx::query("DELETE FROM meeting_state WHERE owner = ?1 AND uid = ?2")
            .bind(owner)
            .bind(uid)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn upsert_meeting_attendee(&self, params: &MeetingAttendeeParams<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO meeting_attendee (meeting_uid, owner, email, name, status, role, response_time, proposed_start, proposed_end, sequence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(owner, meeting_uid, email) DO UPDATE SET
             name = ?4, status = ?5, role = ?6, response_time = ?7, proposed_start = ?8, proposed_end = ?9, sequence = ?10, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(params.meeting_uid)
        .bind(params.owner)
        .bind(params.email)
        .bind(params.name)
        .bind(params.status)
        .bind(params.role)
        .bind(params.response_time)
        .bind(params.proposed_start)
        .bind(params.proposed_end)
        .bind(params.sequence)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_meeting_attendees(
        &self,
        owner: &str,
        meeting_uid: &str,
    ) -> Result<Vec<MeetingAttendeeRow>> {
        sqlx::query_as::<_, MeetingAttendeeRow>(
            "SELECT meeting_uid, owner, email, name, status, role, response_time, proposed_start, proposed_end, sequence, created_at, updated_at
             FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2 ORDER BY email ASC"
        )
        .bind(owner)
        .bind(meeting_uid)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn delete_meeting_attendee(
        &self,
        owner: &str,
        meeting_uid: &str,
        email: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2 AND email = ?3",
        )
        .bind(owner)
        .bind(meeting_uid)
        .bind(email)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn delete_meeting_attendees(&self, owner: &str, meeting_uid: &str) -> Result<()> {
        sqlx::query("DELETE FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2")
            .bind(owner)
            .bind(meeting_uid)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn enqueue_scheduling(
        &self,
        owner: &str,
        meeting_uid: &str,
        operation: &str,
        sequence: u32,
        ical_data: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO meeting_scheduling_queue (meeting_uid, owner, operation, sequence, ical_data, status, attempts) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0)"
        )
        .bind(meeting_uid)
        .bind(owner)
        .bind(operation)
        .bind(sequence)
        .bind(ical_data)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_pending_scheduling(
        &self,
        owner: &str,
        limit: i64,
    ) -> Result<Vec<SchedulingQueueRow>> {
        sqlx::query_as::<_, SchedulingQueueRow>(
            "SELECT id, meeting_uid, owner, operation, sequence, ical_data, status, attempts, last_attempt, error_message, created_at, processed_at
             FROM meeting_scheduling_queue WHERE owner = ?1 AND status = 'pending' ORDER BY id ASC LIMIT ?2"
        )
        .bind(owner)
        .bind(limit.min(100))
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn mark_scheduling_processed(
        &self,
        id: i64,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE meeting_scheduling_queue SET status = ?1, error_message = ?2, attempts = attempts + 1, last_attempt = CURRENT_TIMESTAMP, processed_at = CURRENT_TIMESTAMP WHERE id = ?3"
        )
        .bind(status)
        .bind(error_message)
        .bind(id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_meetings_by_time_range(
        &self,
        owner: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<MeetingStateRow>> {
        sqlx::query_as::<_, MeetingStateRow>(
            "SELECT uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone, created_at, updated_at, last_sequence_time
             FROM meeting_state WHERE owner = ?1 AND start_time >= ?2 AND end_time <= ?3 ORDER BY start_time ASC"
        )
        .bind(owner)
        .bind(start)
        .bind(end)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))
    }

    pub async fn upsert_calendar_attachment(
        &self,
        attachment: &crate::attachment::AttachmentRecord,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO calendar_attachment (id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
             parent_item_server_id = ?2, name = ?4, content_type = ?5, content_size = ?6, content_base64 = ?7, is_inline = ?8,
             content_id = ?9, content_location = ?10, attachment_type = ?11, last_modified_time = ?12, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(&attachment.id)
        .bind(&attachment.parent_item_server_id)
        .bind(&attachment.owner)
        .bind(&attachment.name)
        .bind(&attachment.content_type)
        .bind(attachment.content_size)
        .bind(&attachment.content_base64)
        .bind(attachment.is_inline as i32)
        .bind(&attachment.content_id)
        .bind(&attachment.content_location)
        .bind(attachment.attachment_type.as_str())
        .bind(&attachment.last_modified_time)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_calendar_attachment(
        &self,
        owner: &str,
        attachment_id: &str,
    ) -> Result<Option<crate::attachment::AttachmentRecord>> {
        let row = sqlx::query(
            "SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time, created_at, updated_at
             FROM calendar_attachment WHERE owner = ?1 AND id = ?2"
        )
        .bind(owner)
        .bind(attachment_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(row.map(|r| crate::attachment::AttachmentRecord {
            id: r.get(0),
            parent_item_server_id: r.get(1),
            owner: r.get(2),
            name: r.get(3),
            content_type: r.get(4),
            content_size: r.get(5),
            content_base64: r.get(6),
            is_inline: r.get::<i32, _>(7) != 0,
            content_id: r.get(8),
            content_location: r.get(9),
            attachment_type: r
                .get::<Option<String>, _>(10)
                .map(|s| crate::attachment::AttachmentType::from(s.as_str()))
                .unwrap_or_default(),
            last_modified_time: r.get(11),
            created_at: r.get(12),
            updated_at: r.get(13),
        }))
    }

    pub async fn get_calendar_attachments_for_item(
        &self,
        owner: &str,
        parent_item_server_id: &str,
    ) -> Result<Vec<crate::attachment::AttachmentRecord>> {
        let rows = sqlx::query(
            "SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time, created_at, updated_at
             FROM calendar_attachment WHERE owner = ?1 AND parent_item_server_id = ?2"
        )
        .bind(owner)
        .bind(parent_item_server_id)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| crate::attachment::AttachmentRecord {
                id: r.get(0),
                parent_item_server_id: r.get(1),
                owner: r.get(2),
                name: r.get(3),
                content_type: r.get(4),
                content_size: r.get(5),
                content_base64: r.get(6),
                is_inline: r.get::<i32, _>(7) != 0,
                content_id: r.get(8),
                content_location: r.get(9),
                attachment_type: r
                    .get::<Option<String>, _>(10)
                    .map(|s| crate::attachment::AttachmentType::from(s.as_str()))
                    .unwrap_or_default(),
                last_modified_time: r.get(11),
                created_at: r.get(12),
                updated_at: r.get(13),
            })
            .collect())
    }

    pub async fn delete_calendar_attachment(&self, owner: &str, attachment_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM calendar_attachment WHERE owner = ?1 AND id = ?2")
            .bind(owner)
            .bind(attachment_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn upsert_room_list(&self, room_list: &crate::room::RoomListRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO room_list (id, email, name) VALUES (?1, ?2, ?3)
             ON CONFLICT(email) DO UPDATE SET name = ?3, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(&room_list.id)
        .bind(&room_list.email)
        .bind(&room_list.name)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_room_lists(&self, _owner: &str) -> Result<Vec<crate::room::RoomListRecord>> {
        let rows = sqlx::query(
            "SELECT id, email, name, created_at, updated_at FROM room_list ORDER BY name ASC",
        )
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| crate::room::RoomListRecord {
                id: r.get(0),
                email: r.get(1),
                name: r.get(2),
                created_at: r.get(3),
                updated_at: r.get(4),
            })
            .collect())
    }

    pub async fn delete_room_list(&self, _owner: &str, email: &str) -> Result<()> {
        sqlx::query("DELETE FROM room_list WHERE email = ?1")
            .bind(email)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_all_rooms(&self, _owner: &str) -> Result<Vec<crate::room::RoomRecord>> {
        let rows = sqlx::query("SELECT id, room_list_email, email, name, capacity, is_available, created_at, updated_at FROM room ORDER BY name ASC")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| crate::room::RoomRecord {
                id: r.get(0),
                room_list_email: r.get(1),
                email: r.get(2),
                name: r.get(3),
                capacity: r.get(4),
                is_available: r.get::<i32, _>(5) != 0,
                created_at: r.get(6),
                updated_at: r.get(7),
            })
            .collect())
    }

    pub async fn delete_room(&self, _owner: &str, email: &str) -> Result<()> {
        sqlx::query("DELETE FROM room WHERE email = ?1")
            .bind(email)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn upsert_room(&self, room: &crate::room::RoomRecord) -> Result<()> {
        sqlx::query(
            "INSERT INTO room (id, room_list_email, email, name, capacity, is_available) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(email) DO UPDATE SET room_list_email = ?2, name = ?4, capacity = ?5, is_available = ?6, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(&room.id)
        .bind(&room.room_list_email)
        .bind(&room.email)
        .bind(&room.name)
        .bind(room.capacity)
        .bind(room.is_available as i32)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    pub async fn get_rooms_for_list(
        &self,
        _owner: &str,
        room_list_email: &str,
    ) -> Result<Vec<crate::room::RoomRecord>> {
        let rows = sqlx::query(
            "SELECT id, room_list_email, email, name, capacity, is_available, created_at, updated_at FROM room WHERE room_list_email = ?1 ORDER BY name ASC"
        )
        .bind(room_list_email)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;

        Ok(rows
            .into_iter()
            .map(|r| crate::room::RoomRecord {
                id: r.get(0),
                room_list_email: r.get(1),
                email: r.get(2),
                name: r.get(3),
                capacity: r.get(4),
                is_available: r.get::<i32, _>(5) != 0,
                created_at: r.get(6),
                updated_at: r.get(7),
            })
            .collect())
    }

    // Contact storage methods using contact_map table

    /// Insert a new contact into contact_map, returning the assigned server_id.
    pub async fn insert_contact(
        &self,
        owner: &str,
        carddav_href: &str,
        server_id: &str,
        etag: Option<&str>,
        vcard: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO contact_map (owner, carddav_href, server_id, etag, vcard) VALUES (?1, ?2, ?3, ?4, ?5)"
        )
        .bind(owner)
        .bind(carddav_href)
        .bind(server_id)
        .bind(etag)
        .bind(vcard)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Update an existing contact's vcard and etag by server_id.
    pub async fn update_contact(
        &self,
        owner: &str,
        server_id: &str,
        etag: Option<&str>,
        vcard: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE contact_map SET etag = ?2, vcard = ?3, updated_at = CURRENT_TIMESTAMP WHERE owner = ?1 AND server_id = ?4"
        )
        .bind(owner)
        .bind(etag)
        .bind(vcard)
        .bind(server_id)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Upsert a contact: insert or update based on owner+carddav_href uniqueness.
    /// If a contact with the same owner and href exists, update its server_id, etag, vcard.
    /// Otherwise insert a new mapping.
    pub async fn upsert_contact(
        &self,
        owner: &str,
        carddav_href: &str,
        server_id: &str,
        etag: Option<&str>,
        vcard: Option<&str>,
    ) -> Result<()> {
        // Use INSERT ... ON CONFLICT to atomically upsert, avoiding race conditions
        sqlx::query(
            "INSERT INTO contact_map (owner, carddav_href, server_id, etag, vcard) VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(owner, carddav_href) DO UPDATE SET \
             server_id = excluded.server_id, \
             etag = excluded.etag, \
             vcard = excluded.vcard, \
             updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(carddav_href)
        .bind(server_id)
        .bind(etag)
        .bind(vcard)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Delete a contact by server_id.
    pub async fn delete_contact(&self, owner: &str, server_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM contact_map WHERE owner = ?1 AND server_id = ?2")
            .bind(owner)
            .bind(server_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Fetch a contact by server_id.
    pub async fn get_contact(&self, owner: &str, server_id: &str) -> Result<Option<ContactRow>> {
        let row = sqlx::query_as::<_, ContactRow>(
            "SELECT id, owner, carddav_href, server_id, etag, vcard, updated_at FROM contact_map WHERE owner = ?1 AND server_id = ?2"
        )
        .bind(owner)
        .bind(server_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row)
    }

    /// Fetch a contact by its CardDAV href.
    pub async fn get_contact_by_href(
        &self,
        owner: &str,
        carddav_href: &str,
    ) -> Result<Option<ContactRow>> {
        let row = sqlx::query_as::<_, ContactRow>(
            "SELECT id, owner, carddav_href, server_id, etag, vcard, updated_at FROM contact_map WHERE owner = ?1 AND carddav_href = ?2"
        )
        .bind(owner)
        .bind(carddav_href)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row)
    }

    /// Get sync token for contacts collection (reuses the sync_state table with collection_id = "8").
    pub async fn get_contacts_sync_token(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<String>> {
        match self.get_sync_key(owner, &format!("8::{}", device_id)).await {
            Ok(Some((sync_key, _token))) => Ok(Some(sync_key)),
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Set sync token for contacts collection.
    pub async fn set_contacts_sync_token(
        &self,
        owner: &str,
        device_id: &str,
        sync_key: &str,
    ) -> Result<()> {
        self.set_sync_key(owner, &format!("8::{}", device_id), sync_key, None)
            .await
    }

    /// Set the EAS Contacts sync state using the generic sync_state table.
    /// The collection_id for contacts is typically "8" scoped by device.
    pub async fn set_contacts_sync_state(
        &self,
        username: &str,
        state_collection_id: &str,
        sync_key: &str,
    ) -> Result<()> {
        self.set_sync_key(username, state_collection_id, sync_key, None)
            .await
    }

    /// Get all contacts for an owner (for sync diffing).
    pub async fn get_all_contacts_for_owner(&self, owner: &str) -> Result<Vec<ContactRow>> {
        let rows = sqlx::query_as::<_, ContactRow>(
            "SELECT id, owner, carddav_href, server_id, etag, vcard, updated_at FROM contact_map WHERE owner = ?1"
        )
        .bind(owner)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
    }

    /// Upsert a gateway-local task and record a change-journal event so EAS
    /// Ping/direct-push and change tracking observe the mutation.
    pub async fn upsert_task(
        &self,
        owner: &str,
        server_id: &str,
        fields: &TaskFields<'_>,
    ) -> Result<()> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        sqlx::query(
            "INSERT INTO task_map (owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body, caldav_href, etag, uid) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18) \
             ON CONFLICT(owner, server_id) DO UPDATE SET \
             subject = excluded.subject, importance = excluded.importance, sensitivity = excluded.sensitivity, \
             start_date = excluded.start_date, due_date = excluded.due_date, utc_start_date = excluded.utc_start_date, \
             utc_due_date = excluded.utc_due_date, complete = excluded.complete, date_completed = excluded.date_completed, \
             reminder_set = excluded.reminder_set, reminder_time = excluded.reminder_time, categories = excluded.categories, \
             body = excluded.body, caldav_href = excluded.caldav_href, etag = excluded.etag, uid = excluded.uid, \
             updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(server_id)
        .bind(fields.subject)
        .bind(fields.importance)
        .bind(fields.sensitivity)
        .bind(fields.start_date)
        .bind(fields.due_date)
        .bind(fields.utc_start_date)
        .bind(fields.utc_due_date)
        .bind(fields.complete)
        .bind(fields.date_completed)
        .bind(fields.reminder_set)
        .bind(fields.reminder_time)
        .bind(fields.categories)
        .bind(fields.body)
        .bind(fields.caldav_href)
        .bind(fields.etag)
        .bind(fields.uid)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        sqlx::query(
            "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'upsert', 'task')"
        )
        .bind(owner)
        .bind(server_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(())
    }

    /// Delete a gateway-local task and record a change-journal tombstone.
    ///
    /// Returns the number of rows actually deleted (0 if the `server_id` was
    /// unknown). The tombstone is only written when a row was removed, and is
    /// tagged with `resource_href = 'task'` so EAS Ping can scope change
    /// detection per content class without misclassifying deletes.
    pub async fn delete_task(&self, owner: &str, server_id: &str) -> Result<u64> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        let deleted = sqlx::query("DELETE FROM task_map WHERE owner = ?1 AND server_id = ?2")
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        if deleted.rows_affected() > 0 {
            sqlx::query(
                "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'delete', 'task')"
            )
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        }

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(deleted.rows_affected())
    }

    /// Fetch a single task by server_id.
    pub async fn get_task(&self, owner: &str, server_id: &str) -> Result<Option<TaskRow>> {
        let row = sqlx::query_as::<_, TaskRow>(
            "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body, caldav_href, etag, uid, updated_at FROM task_map WHERE owner = ?1 AND server_id = ?2"
        )
        .bind(owner)
        .bind(server_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row)
    }

    /// Get all tasks for an owner.
    pub async fn get_all_tasks_for_owner(&self, owner: &str) -> Result<Vec<TaskRow>> {
        let rows = sqlx::query_as::<_, TaskRow>(
            "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body, caldav_href, etag, uid, updated_at FROM task_map WHERE owner = ?1 ORDER BY server_id ASC"
        )
        .bind(owner)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
    }

    /// Update only the backend identity (caldav_href, etag, uid) of a task.
    ///
    /// This is mirror bookkeeping after the backing VTODO was created, moved,
    /// or rewritten server-side: the EAS-visible content projection did not
    /// change, so — unlike `upsert_task` — no change-journal event is written
    /// and clients are not told about a mutation they already have.
    pub async fn set_task_backend_ref(
        &self,
        owner: &str,
        server_id: &str,
        caldav_href: &str,
        etag: Option<&str>,
        uid: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE task_map SET caldav_href = ?3, etag = ?4, uid = COALESCE(?5, uid), updated_at = CURRENT_TIMESTAMP \
             WHERE owner = ?1 AND server_id = ?2",
        )
        .bind(owner)
        .bind(server_id)
        .bind(caldav_href)
        .bind(etag)
        .bind(uid)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Upsert a gateway-local note and record a change-journal event.
    pub async fn upsert_note(
        &self,
        owner: &str,
        server_id: &str,
        fields: &NoteFields<'_>,
    ) -> Result<()> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        sqlx::query(
            "INSERT INTO note_map (owner, server_id, subject, message_class, body, categories, last_modified_date, caldav_href, etag, uid) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(owner, server_id) DO UPDATE SET \
             subject = excluded.subject, message_class = excluded.message_class, body = excluded.body, \
             categories = excluded.categories, last_modified_date = excluded.last_modified_date, \
             caldav_href = excluded.caldav_href, etag = excluded.etag, uid = excluded.uid, \
             updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(server_id)
        .bind(fields.subject)
        .bind(fields.message_class)
        .bind(fields.body)
        .bind(fields.categories)
        .bind(fields.last_modified_date)
        .bind(fields.caldav_href)
        .bind(fields.etag)
        .bind(fields.uid)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        sqlx::query(
            "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'upsert', 'note')"
        )
        .bind(owner)
        .bind(server_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(())
    }

    /// Delete a gateway-local note and record a change-journal tombstone.
    ///
    /// Returns the number of rows actually deleted (0 if the `server_id` was
    /// unknown). The tombstone is only written when a row was removed, and is
    /// tagged with `resource_href = 'note'` for class-scoped Ping detection.
    pub async fn delete_note(&self, owner: &str, server_id: &str) -> Result<u64> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Transaction error: {}", e)))?;

        let deleted = sqlx::query("DELETE FROM note_map WHERE owner = ?1 AND server_id = ?2")
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;

        if deleted.rows_affected() > 0 {
            sqlx::query(
                "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'delete', 'note')"
            )
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        }

        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("Commit error: {}", e)))?;
        Ok(deleted.rows_affected())
    }

    /// Fetch a single note by server_id.
    pub async fn get_note(&self, owner: &str, server_id: &str) -> Result<Option<NoteRow>> {
        let row = sqlx::query_as::<_, NoteRow>(
            "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, caldav_href, etag, uid, updated_at FROM note_map WHERE owner = ?1 AND server_id = ?2"
        )
        .bind(owner)
        .bind(server_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row)
    }

    /// Get all notes for an owner.
    pub async fn get_all_notes_for_owner(&self, owner: &str) -> Result<Vec<NoteRow>> {
        let rows = sqlx::query_as::<_, NoteRow>(
            "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, caldav_href, etag, uid, updated_at FROM note_map WHERE owner = ?1 ORDER BY server_id ASC"
        )
        .bind(owner)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
    }

    /// Update only the backend identity (caldav_href, etag, uid) of a note.
    ///
    /// Mirror bookkeeping, not an EAS-visible mutation: no journal event is
    /// written (see `set_task_backend_ref`).
    pub async fn set_note_backend_ref(
        &self,
        owner: &str,
        server_id: &str,
        caldav_href: &str,
        etag: Option<&str>,
        uid: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE note_map SET caldav_href = ?3, etag = ?4, uid = COALESCE(?5, uid), updated_at = CURRENT_TIMESTAMP \
             WHERE owner = ?1 AND server_id = ?2",
        )
        .bind(owner)
        .bind(server_id)
        .bind(caldav_href)
        .bind(etag)
        .bind(uid)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("DB error: {}", e)))?;
        Ok(())
    }

    /// Fetch the gateway-local photo for `owner` (normalized SMTP address).
    ///
    /// Returns `None` when no photo has been stored. This is an *opt-in* store:
    /// Stalwart exposes no avatar object, so only photos an operator (or a
    /// future `SetUserPhoto` surface) explicitly persisted are ever returned.
    /// It never consults the directory, preserving the disclosure-free
    /// `GetUserPhoto` behaviour (no account-set enumeration).
    pub async fn get_user_photo(&self, owner: &str) -> Result<Option<UserPhotoRow>> {
        let row = sqlx::query_as::<_, UserPhotoRow>(
            "SELECT owner, data, content_type, updated_at FROM user_photo WHERE owner = ?1",
        )
        .bind(owner)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(row)
    }

    /// Upsert a gateway-local photo for `owner`, replacing any existing blob.
    pub async fn set_user_photo(&self, owner: &str, data: &[u8], content_type: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_photo (owner, data, content_type, updated_at) \
             VALUES (?1, ?2, ?3, CURRENT_TIMESTAMP) \
             ON CONFLICT(owner) DO UPDATE SET data = excluded.data, \
               content_type = excluded.content_type, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(data)
        .bind(content_type)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(())
    }

    /// Remove the gateway-local photo for `owner`, if present.
    pub async fn delete_user_photo(&self, owner: &str) -> Result<()> {
        sqlx::query("DELETE FROM user_photo WHERE owner = ?1")
            .bind(owner)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(())
    }

    /// Upsert one harvested/S/MIME seed certificate keyed by the email
    /// identity it claims. Re-seeing the same certificate refreshes
    /// `last_seen` only; the DER blob and validity window are immutable by
    /// definition (the fingerprint keys the row).
    pub async fn put_smime_cert(&self, rec: &crate::smime::SmimeCertificate) -> Result<()> {
        sqlx::query(
            "INSERT INTO smime_cert (email, sha256, cert_der, not_before_unix, not_after_unix, last_seen) \
             VALUES (?1, ?2, ?3, ?4, ?5, CURRENT_TIMESTAMP) \
             ON CONFLICT(email, sha256) DO UPDATE SET last_seen = CURRENT_TIMESTAMP",
        )
        .bind(rec.email.to_lowercase())
        .bind(&rec.sha256)
        .bind(&rec.cert_der)
        .bind(rec.not_before_unix)
        .bind(rec.not_after_unix)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(())
    }

    /// All currently-valid stored certificates for `email` (lowercased),
    /// newest `not_after` first. Expired/not-yet-valid certificates are
    /// excluded — per MS-ASCMD §2.2.3.177.12 a recipient with no valid
    /// certificate must surface Certificates Status 7, not stale material.
    pub async fn get_smime_certs(&self, email: &str, now_unix: i64) -> Result<Vec<SmimeCertRow>> {
        let rows = sqlx::query_as::<_, SmimeCertRow>(
            "SELECT sha256, cert_der, not_before_unix, not_after_unix FROM smime_cert \
             WHERE email = ?1 AND not_before_unix <= ?2 AND not_after_unix >= ?2 \
             ORDER BY not_after_unix DESC",
        )
        .bind(email.to_lowercase())
        .bind(now_unix)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
    }
}

/// Row of the gateway-local S/MIME certificate store.
#[derive(FromRow, Clone)]
pub struct SmimeCertRow {
    pub sha256: String,
    pub cert_der: Vec<u8>,
    pub not_before_unix: i64,
    pub not_after_unix: i64,
}

/// Column values for a task upsert (parsed EAS/MS-ASTASK projection of a
/// VTODO plus its backend identity).
#[derive(Default)]
pub struct TaskFields<'a> {
    pub subject: Option<&'a str>,
    pub importance: Option<i64>,
    pub sensitivity: Option<i64>,
    pub start_date: Option<&'a str>,
    pub due_date: Option<&'a str>,
    pub utc_start_date: Option<&'a str>,
    pub utc_due_date: Option<&'a str>,
    pub complete: i64,
    pub date_completed: Option<&'a str>,
    pub reminder_set: i64,
    pub reminder_time: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub body: Option<&'a str>,
    /// CalDAV resource path of the backing VTODO (None until first pushed).
    pub caldav_href: Option<&'a str>,
    /// Server-issued ETag of the last observed VTODO revision.
    pub etag: Option<&'a str>,
    /// iCalendar UID of the backing VTODO.
    pub uid: Option<&'a str>,
}

/// Column values for a note upsert (parsed EAS/MS-ASNOTE projection of a
/// VJOURNAL plus its backend identity).
#[derive(Default)]
pub struct NoteFields<'a> {
    pub subject: Option<&'a str>,
    pub message_class: Option<&'a str>,
    pub body: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub last_modified_date: Option<&'a str>,
    /// CalDAV resource path of the backing VJOURNAL (None until first pushed).
    pub caldav_href: Option<&'a str>,
    /// Server-issued ETag of the last observed VJOURNAL revision.
    pub etag: Option<&'a str>,
    /// iCalendar UID of the backing VJOURNAL.
    pub uid: Option<&'a str>,
}

/// Row for a persisted EWS UserConfiguration object (MS-OXWSUSRCFG).
pub struct UserConfigRow {
    pub folder_id: String,
    pub name: String,
    pub dictionary: Option<String>,
    pub xml_data: Option<String>,
    pub binary_data: Option<String>,
    pub change_key: i64,
}

impl Storage {
    /// Fetch a persisted UserConfiguration object by (owner, folder, name).
    pub async fn get_user_config(
        &self,
        owner: &str,
        folder_id: &str,
        name: &str,
    ) -> Result<Option<UserConfigRow>> {
        let row = sqlx::query(
            "SELECT folder_id, name, dictionary, xml_data, binary_data, change_key
             FROM user_config WHERE owner = ?1 AND folder_id = ?2 AND name = ?3",
        )
        .bind(owner)
        .bind(folder_id)
        .bind(name)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("user_config query error: {}", e)))?;
        Ok(row.map(|r| UserConfigRow {
            folder_id: r.get(0),
            name: r.get(1),
            dictionary: r.get(2),
            xml_data: r.get(3),
            binary_data: r.get(4),
            change_key: r.get(5),
        }))
    }

    /// Insert or replace a UserConfiguration object, bumping change_key.
    /// Returns the new change_key.
    pub async fn put_user_config(
        &self,
        owner: &str,
        folder_id: &str,
        name: &str,
        dictionary: Option<&str>,
        xml_data: Option<&str>,
        binary_data: Option<&str>,
    ) -> Result<i64> {
        // Upsert and change_key readback must be one transaction: a
        // concurrent writer committing in between would let us read its
        // change_key instead of the one this call produced.
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| GatewayError::Storage(format!("user_config transaction error: {}", e)))?;
        sqlx::query(
            "INSERT INTO user_config (owner, folder_id, name, dictionary, xml_data, binary_data, change_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)
             ON CONFLICT(owner, folder_id, name) DO UPDATE SET
               dictionary = ?4, xml_data = ?5, binary_data = ?6,
               change_key = change_key + 1, updated_at = CURRENT_TIMESTAMP",
        )
        .bind(owner)
        .bind(folder_id)
        .bind(name)
        .bind(dictionary)
        .bind(xml_data)
        .bind(binary_data)
        .execute(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("user_config upsert error: {}", e)))?;
        let row: (i64,) = sqlx::query_as(
            "SELECT change_key FROM user_config WHERE owner = ?1 AND folder_id = ?2 AND name = ?3",
        )
        .bind(owner)
        .bind(folder_id)
        .bind(name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| GatewayError::Storage(format!("user_config readback error: {}", e)))?;
        tx.commit()
            .await
            .map_err(|e| GatewayError::Storage(format!("user_config commit error: {}", e)))?;
        Ok(row.0)
    }

    /// Delete a UserConfiguration object. Returns true when a row existed.
    pub async fn delete_user_config(
        &self,
        owner: &str,
        folder_id: &str,
        name: &str,
    ) -> Result<bool> {
        let res = sqlx::query(
            "DELETE FROM user_config WHERE owner = ?1 AND folder_id = ?2 AND name = ?3",
        )
        .bind(owner)
        .bind(folder_id)
        .bind(name)
        .execute(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("user_config delete error: {}", e)))?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_storage() -> Storage {
        let storage = Storage::new("sqlite::memory:")
            .await
            .expect("in-memory storage");
        storage.init_schema().await.expect("schema init");
        storage
    }

    async fn journal_len(storage: &Storage, owner: &str) -> usize {
        storage
            .list_journal_since_seq(owner, 0)
            .await
            .expect("journal listing")
            .len()
    }

    #[tokio::test]
    async fn prune_change_journal_removes_rows_below_all_watermarks() {
        let storage = mem_storage().await;
        let owner = "prune-user@example.com";
        for i in 0..5 {
            storage
                .upsert_item_map(
                    owner,
                    &format!("http://cal/{i}"),
                    "calendar/",
                    &format!("cal-{i}"),
                    &format!("uid-{i}"),
                    "e",
                )
                .await
                .expect("journal upsert");
        }
        assert_eq!(journal_len(&storage, owner).await, 5);

        // Oldest device syncs at seq 3; everything at/below it is unreadable.
        storage
            .set_sync_key(owner, "1::device-b", "key-b", Some("seq:3"))
            .await
            .expect("store watermark");
        storage
            .set_sync_key(owner, "1::device-a", "key-a", Some("seq:5"))
            .await
            .expect("store watermark");
        // A non-seq token (JMAP state on the email folder) must not confuse
        // the watermark scan.
        storage
            .set_sync_key(owner, "4::device-a", "key-e", Some("s-1234"))
            .await
            .expect("store JMAP token");

        let removed = storage
            .prune_change_journal(owner)
            .await
            .expect("prune must succeed");
        assert_eq!(removed, 3, "rows 1..=3 are below every device watermark");
        assert_eq!(journal_len(&storage, owner).await, 2);

        // Ids keep advancing — AUTOINCREMENT never reuses pruned ids.
        storage
            .upsert_item_map(owner, "http://cal/9", "calendar/", "cal-9", "uid-9", "e9")
            .await
            .expect("journal upsert");
        assert_eq!(
            storage.get_latest_change_seq().await.expect("head"),
            6,
            "journal ids remain monotonic after pruning"
        );
    }

    #[tokio::test]
    async fn prune_change_journal_without_watermark_keeps_everything() {
        let storage = mem_storage().await;
        let owner = "never-synced@example.com";
        storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        // Malformed seq-shaped token (no digits) must also block pruning.
        storage
            .set_sync_key(owner, "1::dev", "key", Some("seq:"))
            .await
            .expect("store malformed watermark");
        assert_eq!(
            storage.prune_change_journal(owner).await.expect("prune"),
            0,
            "no usable watermark => full journal retained for restart replay"
        );
        assert_eq!(journal_len(&storage, owner).await, 1);
    }

    #[tokio::test]
    async fn prune_change_journals_only_touches_the_owning_mailbox() {
        let storage = mem_storage().await;
        for i in 0..3 {
            storage
                .upsert_item_map(
                    "a@example.com",
                    &format!("h{i}a"),
                    "calendar/",
                    &format!("a-{i}"),
                    "u",
                    "e",
                )
                .await
                .expect("journal a");
            storage
                .upsert_item_map(
                    "b@example.com",
                    &format!("h{i}b"),
                    "calendar/",
                    &format!("b-{i}"),
                    "u",
                    "e",
                )
                .await
                .expect("journal b");
        }
        storage
            .set_sync_key("a@example.com", "1::dev", "key", Some("seq:100"))
            .await
            .expect("watermark a");
        // b has no watermark: fully retained.
        let removed = storage.prune_change_journals().await.expect("prune all");
        assert_eq!(removed, 3, "only owner a's rows are removable");
        assert_eq!(journal_len(&storage, "b@example.com").await, 3);
    }

    #[tokio::test]
    async fn journal_watermark_combines_seq_token_and_column() {
        let storage = mem_storage().await;
        let owner = "wm-user@example.com";

        // No row at all.
        assert_eq!(
            storage
                .journal_watermark(owner, "1::dev")
                .await
                .expect("watermark query"),
            None
        );

        // Column-only watermark (provider state token — e.g. JMAP query_state).
        storage
            .set_sync_key(owner, "1::dev", "key", Some("query-state-x"))
            .await
            .expect("sync key");
        storage
            .set_journal_watermark(owner, "1::dev", 5)
            .await
            .expect("watermark");
        assert_eq!(
            storage
                .journal_watermark(owner, "1::dev")
                .await
                .expect("watermark query"),
            Some(5)
        );

        // Both present: the higher value wins (a stale seq token must not
        // resurrect already-journaled rows).
        storage
            .set_sync_key(owner, "1::dev", "key2", Some("seq:3"))
            .await
            .expect("sync key");
        assert_eq!(
            storage
                .journal_watermark(owner, "1::dev")
                .await
                .expect("watermark query"),
            Some(5)
        );
        // And seq tokens still count when higher than the column.
        storage
            .set_sync_key(owner, "1::dev", "key3", Some("seq:9"))
            .await
            .expect("sync key");
        assert_eq!(
            storage
                .journal_watermark(owner, "1::dev")
                .await
                .expect("watermark query"),
            Some(9)
        );
    }

    /// §16: pruning records the deleted floor per owner, and a sync resuming
    /// from below it is detected as stale (its delta window is gone). A
    /// watermark at exactly the floor is intact; the recorded floor only ever
    /// advances.
    #[tokio::test]
    async fn prune_records_floor_and_detects_stale_windows() {
        let storage = mem_storage().await;
        let owner = "stale-wm@example.com";
        for i in 0..5 {
            storage
                .upsert_item_map(
                    owner,
                    &format!("http://cal/{i}"),
                    "calendar/",
                    &format!("cal-{i}"),
                    &format!("uid-{i}"),
                    "e",
                )
                .await
                .expect("journal upsert");
        }

        // Nothing pruned yet: every position is replayable.
        assert_eq!(storage.pruned_through(owner).await.expect("floor"), 0);
        assert!(
            !storage
                .journal_window_stale(owner, 0)
                .await
                .expect("stale check")
        );

        // Oldest device syncs at seq 3; everything at/below it is pruned.
        storage
            .set_sync_key(owner, "1::dev", "key", Some("seq:3"))
            .await
            .expect("watermark");
        assert_eq!(storage.prune_change_journal(owner).await.expect("prune"), 3);
        assert_eq!(
            storage.pruned_through(owner).await.expect("floor"),
            3,
            "the deleted floor is recorded"
        );

        // A watermark below the floor can no longer reconstruct its delta.
        assert!(
            storage
                .journal_window_stale(owner, 0)
                .await
                .expect("stale check"),
            "seq 0 is below the pruned floor"
        );
        assert!(
            storage
                .journal_window_stale(owner, 2)
                .await
                .expect("stale check"),
            "seq 2 is below the pruned floor"
        );
        // Exactly at the floor the window is intact (rows > floor remain).
        assert!(
            !storage
                .journal_window_stale(owner, 3)
                .await
                .expect("stale check")
        );
        assert!(
            !storage
                .journal_window_stale(owner, 9)
                .await
                .expect("stale check")
        );

        // A later, LOWER-but-positive floor (a fresh device syncing from a low
        // position) must not erase the recorded high-water mark. A `seq:0`
        // watermark would make the second prune's floor 0 and never exercise
        // the `MAX` conflict update, so use a positive value strictly below 3.
        storage
            .set_sync_key(owner, "2::fresh", "k2", Some("seq:1"))
            .await
            .expect("fresh watermark");
        // Rows at id <= 1 were already removed by the first prune, so this one
        // deletes nothing new; the floor upsert still runs and must not lower
        // the recorded high-water mark.
        assert_eq!(
            storage
                .prune_change_journal(owner)
                .await
                .expect("prune again"),
            0,
            "the second prune has no new rows to remove"
        );
        assert_eq!(
            storage.pruned_through(owner).await.expect("floor"),
            3,
            "the recorded floor is monotonic under a lower positive floor"
        );
        assert!(
            storage
                .journal_window_stale(owner, 1)
                .await
                .expect("stale check"),
            "seq 1 is still below the retained floor of 3"
        );
    }

    /// §16: pruning that cannot run (no watermark) must not fabricate a floor;
    /// the full journal stays replayable and no window is reported stale.
    #[tokio::test]
    async fn prune_without_watermark_leaves_floor_unset() {
        let storage = mem_storage().await;
        let owner = "no-wm@example.com";
        storage
            .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
            .await
            .expect("journal upsert");
        assert_eq!(storage.prune_change_journal(owner).await.expect("prune"), 0);
        assert_eq!(storage.pruned_through(owner).await.expect("floor"), 0);
        assert!(
            !storage
                .journal_window_stale(owner, 0)
                .await
                .expect("stale check"),
            "nothing pruned => nothing stale"
        );
    }

    #[tokio::test]
    async fn prune_respects_journal_seq_column_watermarks() {
        let storage = mem_storage().await;
        let owner = "col-wm@example.com";
        for i in 0..4 {
            storage
                .upsert_item_map(
                    owner,
                    &format!("http://cal/{i}"),
                    "calendar/",
                    &format!("cal-{i}"),
                    &format!("uid-{i}"),
                    "e",
                )
                .await
                .expect("journal upsert");
        }
        // Provider-state token plus a column watermark at 2: rows 1..=2 are
        // unreachable and prunable even though no `seq:` token exists.
        storage
            .set_sync_key(owner, "1::dev", "key", Some("jmap-query-state"))
            .await
            .expect("sync key");
        storage
            .set_journal_watermark(owner, "1::dev", 2)
            .await
            .expect("watermark");
        assert_eq!(storage.prune_change_journal(owner).await.expect("prune"), 2);
        assert_eq!(journal_len(&storage, owner).await, 2);
    }

    /// §16: the conditional cursor clear only removes the exact cursor it was
    /// asked to abandon; a cursor advanced (or replaced by a new prime) in the
    /// meantime is left intact, so a concurrent prime's continuation window is
    /// never dropped by a stale-watermark guard.
    #[tokio::test]
    async fn clear_local_sync_cursor_if_is_compare_and_set() {
        let storage = mem_storage().await;
        let owner = "cas-cursor@example.com";
        let coll = "3::dev";
        storage
            .set_local_sync_cursor(owner, coll, "task-010", 7)
            .await
            .expect("persist cursor");

        // A mismatch (stale observed key) leaves the cursor untouched.
        storage
            .clear_local_sync_cursor_if(owner, coll, "task-005", 7)
            .await
            .expect("conditional clear");
        assert_eq!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("read"),
            Some(("task-010".to_string(), 7)),
            "a stale expected key must not clear a newer cursor"
        );
        // A mismatch on base_seq also leaves it intact.
        storage
            .clear_local_sync_cursor_if(owner, coll, "task-010", 6)
            .await
            .expect("conditional clear");
        assert!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("read")
                .is_some(),
            "a stale expected base_seq must not clear the cursor"
        );
        // An exact match clears it.
        storage
            .clear_local_sync_cursor_if(owner, coll, "task-010", 7)
            .await
            .expect("conditional clear");
        assert!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("read")
                .is_none(),
            "an exact match must clear the cursor"
        );
    }

    /// §16: the keyset enumeration used to re-prime a pruned client lists live
    /// `item_map` rows in server_id order, strictly after the cursor, bounded
    /// by the limit.
    #[tokio::test]
    async fn list_changes_after_server_id_paginates_live_set() {
        let storage = mem_storage().await;
        let owner = "keyset@example.com";
        for id in ["a", "b", "c", "d"] {
            storage
                .upsert_item_map(
                    owner,
                    &format!("http://cal/{id}"),
                    "calendar/",
                    id,
                    &format!("uid-{id}"),
                    "e",
                )
                .await
                .expect("upsert");
        }
        let all = storage
            .list_changes_after_server_id(owner, None, 10)
            .await
            .expect("list");
        let ids: Vec<&str> = all.iter().map(|r| r.server_id.as_str()).collect();
        assert_eq!(ids, ["a", "b", "c", "d"], "server_id ASC order");

        let page = storage
            .list_changes_after_server_id(owner, Some("b"), 10)
            .await
            .expect("list");
        let ids: Vec<&str> = page.iter().map(|r| r.server_id.as_str()).collect();
        assert_eq!(ids, ["c", "d"], "strictly after the cursor");

        let limited = storage
            .list_changes_after_server_id(owner, None, 2)
            .await
            .expect("list");
        let ids: Vec<&str> = limited.iter().map(|r| r.server_id.as_str()).collect();
        assert_eq!(ids, ["a", "b"], "bounded by the limit");
    }

    #[tokio::test]
    async fn local_sync_cursor_roundtrips_base_seq() {
        // Continuation windows read the journal base captured when the
        // initial sync started, so the cursor must persist and return it.
        let storage = mem_storage().await;
        let owner = "cursor-base@example.com";
        let coll = "7::dev";
        assert!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("cursor read")
                .is_none()
        );
        storage
            .set_local_sync_cursor(owner, coll, "task-003", 42)
            .await
            .expect("cursor write");
        assert_eq!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("cursor read"),
            Some(("task-003".to_string(), 42))
        );
        // Re-pointing the keyset cursor keeps updating base_seq atomically.
        storage
            .set_local_sync_cursor(owner, coll, "task-007", 42)
            .await
            .expect("cursor update");
        assert_eq!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("cursor read"),
            Some(("task-007".to_string(), 42))
        );
        storage
            .clear_local_sync_cursor(owner, coll)
            .await
            .expect("cursor clear");
        assert!(
            storage
                .get_local_sync_cursor(owner, coll)
                .await
                .expect("cursor read")
                .is_none()
        );
    }

    #[tokio::test]
    async fn list_tasks_after_honors_limit_for_windowed_fetch() {
        // The continuation window asks for window + 1 rows to detect
        // <MoreAvailable/>; the storage layer must honor the bound instead of
        // loading the whole remaining set.
        let storage = mem_storage().await;
        let owner = "keyset-limit@example.com";
        for i in 0..5 {
            storage
                .upsert_task(
                    owner,
                    &format!("task-{i:03}"),
                    &TaskFields {
                        subject: Some(&format!("Task {i}")),
                        ..Default::default()
                    },
                )
                .await
                .expect("seed task");
        }

        let from_start = storage
            .list_tasks_after(owner, None, 3)
            .await
            .expect("keyset query");
        assert_eq!(from_start.len(), 3);
        assert_eq!(from_start[0].server_id, "task-000");
        assert_eq!(from_start[2].server_id, "task-002");

        let resumed = storage
            .list_tasks_after(owner, Some("task-002"), 3)
            .await
            .expect("keyset query");
        assert_eq!(resumed.len(), 2, "only the remaining rows exist");
        assert_eq!(resumed[0].server_id, "task-003");

        let exact = storage
            .list_tasks_after(owner, None, 5)
            .await
            .expect("keyset query");
        assert_eq!(exact.len(), 5, "a limit past the set size returns all rows");

        let oversized = storage
            .list_tasks_after(owner, None, 10)
            .await
            .expect("keyset query");
        assert_eq!(oversized.len(), 5);
    }

    #[tokio::test]
    async fn init_schema_migrates_pre_base_seq_cursors() {
        // Databases created before the base_seq column carried windowed
        // initial-sync cursors without the journal base; opening them must
        // add the column (defaulting legacy rows to 0) and keep them usable.
        let dir = std::env::temp_dir().join(format!(
            "gateway-storage-cursor-migrate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("gateway.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());

        // Pre-create local_sync_cursor in its pre-migration shape.
        {
            let pool = sqlx::SqlitePool::connect(&url).await.expect("raw pool");
            sqlx::query(
                "CREATE TABLE local_sync_cursor (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner TEXT NOT NULL,
                    collection_id TEXT NOT NULL,
                    last_key TEXT NOT NULL,
                    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(owner, collection_id)
                )",
            )
            .execute(&pool)
            .await
            .expect("legacy cursor table");
            sqlx::query(
                "INSERT INTO local_sync_cursor (owner, collection_id, last_key) VALUES ('o', '7::dev', 'task-042')",
            )
            .execute(&pool)
            .await
            .expect("legacy cursor row");
            pool.close().await;
        }

        let storage = Storage::new(&url).await.expect("open db");
        storage.init_schema().await.expect("schema init migrates");
        // Running init twice must be safe (column now present).
        storage.init_schema().await.expect("re-init idempotent");
        assert_eq!(
            storage
                .get_local_sync_cursor("o", "7::dev")
                .await
                .expect("cursor after migration"),
            Some(("task-042".to_string(), 0)),
            "legacy cursor rows survive with the base_seq default"
        );
        // The migrated cursor stays fully functional.
        storage
            .set_local_sync_cursor("o", "7::dev", "task-050", 9)
            .await
            .expect("cursor update after migration");
        assert_eq!(
            storage
                .get_local_sync_cursor("o", "7::dev")
                .await
                .expect("cursor read"),
            Some(("task-050".to_string(), 9))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn init_schema_migrates_pre_journal_seq_databases() {
        let dir =
            std::env::temp_dir().join(format!("gateway-storage-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("gateway.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());

        // Pre-create sync_state in its pre-migration shape (no journal_seq).
        {
            let pool = sqlx::SqlitePool::connect(&url).await.expect("raw pool");
            sqlx::query(
                "CREATE TABLE sync_state (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner TEXT NOT NULL,
                    collection_id TEXT NOT NULL,
                    sync_key TEXT NOT NULL,
                    token TEXT,
                    protocol_version TEXT DEFAULT '16.1',
                    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(owner, collection_id)
                )",
            )
            .execute(&pool)
            .await
            .expect("legacy table");
            sqlx::query(
                "INSERT INTO sync_state (owner, collection_id, sync_key, token) VALUES ('o', 'c', 'k', 'seq:4')",
            )
            .execute(&pool)
            .await
            .expect("legacy row");
            pool.close().await;
        }

        let storage = Storage::new(&url).await.expect("open db");
        storage.init_schema().await.expect("schema init migrates");
        // Running init twice must be safe (column now present).
        storage.init_schema().await.expect("re-init idempotent");
        assert_eq!(
            storage
                .journal_watermark("o", "c")
                .await
                .expect("watermark after migration"),
            Some(4),
            "existing rows survive the migration"
        );
        storage
            .set_journal_watermark("o", "c", 7)
            .await
            .expect("watermark after migration");
        assert_eq!(
            storage
                .journal_watermark("o", "c")
                .await
                .expect("watermark after migration"),
            Some(7)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn init_schema_migrates_pre_backend_task_note_columns() {
        let dir = std::env::temp_dir().join(format!(
            "gateway-storage-tasknote-migrate-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("gateway.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());

        // Pre-create task_map/note_map in their true pre-§15 shape (the DDL as
        // it stood before this PR): every projected column present, but no
        // caldav_href/etag/uid. The href/uid indexes must NOT be declared as
        // top-level CREATE INDEX statements in the schema file, or schema
        // execution would abort with "no such column" here before the ALTERs.
        {
            let pool = sqlx::SqlitePool::connect(&url).await.expect("raw pool");
            sqlx::query(
                "CREATE TABLE task_map (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner TEXT NOT NULL,
                    server_id TEXT NOT NULL,
                    subject TEXT,
                    importance INTEGER,
                    sensitivity INTEGER,
                    start_date TEXT,
                    due_date TEXT,
                    utc_start_date TEXT,
                    utc_due_date TEXT,
                    complete INTEGER NOT NULL DEFAULT 0,
                    date_completed TEXT,
                    reminder_set INTEGER NOT NULL DEFAULT 0,
                    reminder_time TEXT,
                    categories TEXT,
                    body TEXT,
                    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(owner, server_id)
                )",
            )
            .execute(&pool)
            .await
            .expect("legacy task_map");
            sqlx::query(
                "CREATE TABLE note_map (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    owner TEXT NOT NULL,
                    server_id TEXT NOT NULL,
                    subject TEXT,
                    message_class TEXT,
                    body TEXT,
                    categories TEXT,
                    last_modified_date TEXT,
                    updated_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(owner, server_id)
                )",
            )
            .execute(&pool)
            .await
            .expect("legacy note_map");
            for table in ["task_map", "note_map"] {
                sqlx::query(&format!(
                    "INSERT INTO {table} (owner, server_id, subject) VALUES ('o', 's-1', 'legacy')"
                ))
                .execute(&pool)
                .await
                .expect("legacy row");
            }
            pool.close().await;
        }

        let storage = Storage::new(&url).await.expect("open db");
        storage.init_schema().await.expect("schema init migrates");
        // Running init twice must be safe (columns now present).
        storage.init_schema().await.expect("re-init idempotent");

        // The three backend-identity columns now exist and the legacy row
        // survives with NULL identity (pushed by the first reconcile).
        let task = storage
            .get_task("o", "s-1")
            .await
            .expect("task read")
            .expect("legacy task survives");
        assert!(task.caldav_href.is_none());
        assert!(task.etag.is_none());
        assert!(task.uid.is_none());
        let note = storage
            .get_note("o", "s-1")
            .await
            .expect("note read")
            .expect("legacy note survives");
        assert!(note.caldav_href.is_none());

        // The href/uid indexes exist on both tables.
        for table in ["task_map", "note_map"] {
            let indexes: Vec<String> = sqlx::query(&format!("PRAGMA index_list({table})"))
                .fetch_all(storage.pool.as_ref())
                .await
                .expect("index list")
                .iter()
                .map(|row| row.get::<String, _>("name"))
                .collect();
            assert!(
                indexes.iter().any(|n| n == &format!("idx_{table}_href")),
                "{table} must have the href index: {indexes:?}"
            );
            assert!(
                indexes.iter().any(|n| n == &format!("idx_{table}_uid")),
                "{table} must have the uid index: {indexes:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn change_journal_survives_reopen_for_restart_replay() {
        let dir =
            std::env::temp_dir().join(format!("gateway-storage-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("gateway.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());
        let owner = "restart-user@example.com";

        {
            let storage = Storage::new(&url).await.expect("open db");
            storage.init_schema().await.expect("schema init");
            storage
                .upsert_item_map(owner, "http://cal/1", "calendar/", "cal-1", "uid-1", "e1")
                .await
                .expect("journal upsert");
            storage
                .add_delete_tombstone(owner, "cal-0")
                .await
                .expect("journal delete");
            // Watermark before any journal row: on restart the full state
            // must replay above it.
            storage
                .set_sync_key(owner, "1::dev", "key", Some("seq:0"))
                .await
                .expect("watermark");
        }

        // Simulated container restart: reopen the same database file and
        // replay the journal from the persisted watermark.
        {
            let storage = Storage::new(&url).await.expect("reopen db");
            storage.init_schema().await.expect("schema idempotent");
            assert_eq!(
                storage.get_latest_change_seq().await.expect("head"),
                2,
                "journal ids persist across restart"
            );
            let upserts = storage
                .list_changes_since_seq(owner, 0, 1000)
                .await
                .expect("replay upserts");
            assert_eq!(upserts.len(), 1, "rows after the watermark replay");
            let deletes = storage
                .list_deleted_since_seq(owner, 0)
                .await
                .expect("replay deletes");
            assert_eq!(deletes.len(), 1, "tombstones after the watermark replay");

            storage
                .prune_change_journals()
                .await
                .expect("prune after restart");
            assert_eq!(
                storage.get_latest_change_seq().await.expect("head"),
                2,
                "pruning keeps ids monotonic after restart"
            );
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    /// [MS-ASCMD] §2.2.1.11 duplicate-RSVP idempotence substrate: the store
    /// keys on (owner, uid, instance_key) so a whole-series response and
    /// per-instance responses for the same meeting coexist, a repeated
    /// decision reads back equal, and a changed decision overwrites. The
    /// recorded row also carries the REQUEST SEQUENCE the REPLY answered
    /// (RFC 5546 §3.2.1.4): a reschedule bumps the revision, so the
    /// (decision, sequence) pair — not the decision alone — is the
    /// duplicate-detection key.
    #[tokio::test]
    async fn meeting_rsvp_store_keys_uid_and_instance_and_owner() {
        let storage = mem_storage().await;
        let owner = "attendee@example.com";
        let uid = "evt-1234";

        // Whole-series accept at SEQUENCE 0.
        storage
            .upsert_meeting_rsvp(&MeetingRsvpRecord {
                owner,
                uid,
                instance_key: "",
                decision: 1,
                sequence: 0,
                message_id: Some("<m1@example.com>"),
                calendar_server_id: Some("cal-1"),
            })
            .await
            .expect("series upsert");

        // Same meeting, one instance declined — a separate key.
        storage
            .upsert_meeting_rsvp(&MeetingRsvpRecord {
                owner,
                uid,
                instance_key: "20260710T090000Z",
                decision: 3,
                sequence: 0,
                message_id: Some("<m2@example.com>"),
                calendar_server_id: None,
            })
            .await
            .expect("instance upsert");

        let series = storage
            .get_meeting_rsvp(owner, uid, "")
            .await
            .expect("series read")
            .expect("series row");
        assert_eq!(series.decision, 1);
        assert_eq!(series.sequence, 0);
        assert_eq!(series.message_id.as_deref(), Some("<m1@example.com>"));
        assert_eq!(series.calendar_server_id.as_deref(), Some("cal-1"));

        let instance = storage
            .get_meeting_rsvp(owner, uid, "20260710T090000Z")
            .await
            .expect("instance read")
            .expect("instance row");
        assert_eq!(instance.decision, 3, "instance key is independent");
        assert_eq!(instance.message_id.as_deref(), Some("<m2@example.com>"));

        // A changed decision overwrites the stored row for that key.
        storage
            .upsert_meeting_rsvp(&MeetingRsvpRecord {
                owner,
                uid,
                instance_key: "",
                decision: 2,
                sequence: 0,
                message_id: Some("<m3@example.com>"),
                calendar_server_id: Some("cal-1"),
            })
            .await
            .expect("decision change");
        let changed = storage
            .get_meeting_rsvp(owner, uid, "")
            .await
            .expect("changed read")
            .expect("changed row");
        assert_eq!(changed.decision, 2);
        assert_eq!(changed.message_id.as_deref(), Some("<m3@example.com>"));

        // An organizer reschedule (RFC 5546 §3.2.1.4) raises SEQUENCE: the
        // attendee's unchanged-decision reply to the new revision must
        // overwrite the row, not be swallowed by an equal-decision check.
        storage
            .upsert_meeting_rsvp(&MeetingRsvpRecord {
                owner,
                uid,
                instance_key: "",
                decision: 2,
                sequence: 1,
                message_id: Some("<m4@example.com>"),
                calendar_server_id: Some("cal-1"),
            })
            .await
            .expect("sequence bump");
        let rescheduled = storage
            .get_meeting_rsvp(owner, uid, "")
            .await
            .expect("rescheduled read")
            .expect("rescheduled row");
        assert_eq!(rescheduled.decision, 2);
        assert_eq!(rescheduled.sequence, 1, "sequence follows the revision");
        assert_eq!(rescheduled.message_id.as_deref(), Some("<m4@example.com>"));

        // Owner scoping: another user's RSVP never leaks across accounts.
        let other = storage
            .get_meeting_rsvp("other@example.com", uid, "")
            .await
            .expect("other read");
        assert!(other.is_none(), "no cross-account RSVP leak");
    }

    /// Databases created before the `sequence` column existed (v9 table
    /// created by an earlier build of this same PR's table) must be migrated
    /// in place: `init_schema` probes and ALTERs rather than assuming the
    /// CREATE TABLE from the current schema file ran.
    #[tokio::test]
    async fn meeting_rsvp_sequence_column_migrates_in_place() {
        let storage = mem_storage().await;
        // Simulate the pre-column table shape.
        sqlx::query("DROP TABLE meeting_rsvp")
            .execute(storage.pool.as_ref())
            .await
            .expect("drop");
        sqlx::query(
            "CREATE TABLE meeting_rsvp (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                owner TEXT NOT NULL,
                uid TEXT NOT NULL,
                instance_key TEXT NOT NULL DEFAULT '',
                decision INTEGER NOT NULL,
                message_id TEXT,
                calendar_server_id TEXT,
                responded_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                UNIQUE(owner, uid, instance_key)
            )",
        )
        .execute(storage.pool.as_ref())
        .await
        .expect("legacy table");

        // A legacy row records a delivered RSVP with no sequence.
        sqlx::query("INSERT INTO meeting_rsvp (owner, uid, instance_key, decision, message_id) VALUES ('attendee@example.com', 'evt-9', '', 1, '<m@example.com>')")
            .execute(storage.pool.as_ref())
            .await
            .expect("legacy row");

        storage.init_schema().await.expect("migration");

        let columns = sqlx::query("PRAGMA table_info(meeting_rsvp)")
            .fetch_all(storage.pool.as_ref())
            .await
            .expect("probe");
        assert!(
            columns
                .iter()
                .any(|c| c.get::<String, _>("name") == "sequence"),
            "sequence column added"
        );

        // The legacy row survives with the default sequence.
        let legacy = storage
            .get_meeting_rsvp("attendee@example.com", "evt-9", "")
            .await
            .expect("legacy read")
            .expect("legacy row");
        assert_eq!(legacy.decision, 1);
        assert_eq!(legacy.sequence, 0);

        // And the store keeps working through the migrated table.
        storage
            .upsert_meeting_rsvp(&MeetingRsvpRecord {
                owner: "attendee@example.com",
                uid: "evt-9",
                instance_key: "",
                decision: 2,
                sequence: 3,
                message_id: Some("<new@example.com>"),
                calendar_server_id: None,
            })
            .await
            .expect("post-migration upsert");
        let updated = storage
            .get_meeting_rsvp("attendee@example.com", "evt-9", "")
            .await
            .expect("post-migration read")
            .expect("post-migration row");
        assert_eq!(updated.decision, 2);
        assert_eq!(updated.sequence, 3);
    }

    fn temp_db_url(tag: &str) -> (std::path::PathBuf, String) {
        let dir =
            std::env::temp_dir().join(format!("gateway-storage-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let db = dir.join("gateway.db");
        let url = format!("sqlite://{}?mode=rwc", db.display());
        (dir, url)
    }

    #[tokio::test]
    async fn new_pool_enables_wal_busy_timeout_and_foreign_keys_on_every_connection() {
        // busy_timeout and foreign_keys are PER-CONNECTION pragmas: the
        // sqlite_schema.sql PRAGMA lines only reach the single connection
        // that ran the schema, so the pool must set them via connect options
        // or later pooled connections lose the SQLITE_BUSY grace (AUDIT §19).
        // journal_mode=WAL is file-persistent; assert it took effect too.
        let (_dir, url) = temp_db_url("pragmas");
        let storage = Storage::new(&url).await.expect("storage");
        storage.init_schema().await.expect("init schema");

        // Occupy every pooled connection at once so each live connection —
        // not just the one init_schema happened to run on — is inspected.
        let mut conns = Vec::new();
        for _ in 0..10 {
            conns.push(storage.pool().acquire().await.expect("acquire"));
        }
        for conn in &mut conns {
            let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
                .fetch_one(&mut **conn)
                .await
                .expect("journal_mode");
            assert_eq!(journal_mode, "wal", "every pooled connection sees WAL");
            let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
                .fetch_one(&mut **conn)
                .await
                .expect("busy_timeout");
            assert_eq!(
                busy_timeout,
                Storage::BUSY_TIMEOUT.as_millis() as i64,
                "every pooled connection waits out the writer lock"
            );
            let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                .fetch_one(&mut **conn)
                .await
                .expect("foreign_keys");
            assert_eq!(foreign_keys, 1, "foreign keys enabled per connection");
        }
    }

    #[tokio::test]
    async fn concurrent_sync_writes_and_ping_reads_never_fail_or_stall() {
        // AUDIT §19 load test: N concurrent long-polled Pings each rewriting
        // their watermark while a stream of Sync mutations journals changes.
        // WAL + busy_timeout must keep every one of these writes and reads
        // error-free and bounded (no SQLITE_BUSY serialization spike on the
        // Ping path, no lost watermarks).
        let (_dir, url) = temp_db_url("load");
        let storage = Storage::new(&url).await.expect("storage");
        storage.init_schema().await.expect("init schema");

        const PINGERS: i64 = 16; // concurrent long-polled Ping loops
        const SYNC_ITERS: i64 = 50; // sync mutations per folder during the poll
        const POLL_TICKS: i64 = 40; // watermark re-reads per ping
        let owner = "user@example.com";
        for ping in 0..PINGERS {
            storage
                .set_sync_key(owner, &format!("folder-{ping}"), "0", None)
                .await
                .expect("prime sync key");
        }

        let start = std::time::Instant::now();
        let mut handles = Vec::new();
        // Ping loops: poll the watermark rewrite path exactly like the Ping
        // per-folder journal check does on every tick.
        for ping in 0..PINGERS {
            let storage = storage.clone();
            handles.push(tokio::spawn(async move {
                let folder = format!("folder-{ping}");
                for _ in 0..POLL_TICKS {
                    storage
                        .record_journal_watermark(owner, &folder)
                        .await
                        .expect("ping watermark write");
                    storage
                        .list_journal_since_seq(owner, 0)
                        .await
                        .expect("ping journal read");
                }
            }));
        }
        // Concurrent Sync loops: journal-appending item writes.
        for ping in 0..PINGERS {
            let storage = storage.clone();
            handles.push(tokio::spawn(async move {
                let folder = format!("folder-{ping}");
                for i in 0..SYNC_ITERS {
                    storage
                        .upsert_item_map(
                            owner,
                            "cal/user/evt.ics",
                            &format!("res-{ping}-{i}"),
                            &format!("srv-{ping}-{i}"),
                            "uid",
                            "\"etag\"",
                        )
                        .await
                        .expect("sync journal write");
                    storage
                        .set_sync_key(owner, &folder, &format!("key-{i}"), None)
                        .await
                        .expect("sync key write");
                }
            }));
        }
        for handle in handles {
            handle.await.expect("worker panicked");
        }
        let elapsed = start.elapsed();

        // Every journal row landed exactly once: no busy-error loss.
        let head = storage.get_latest_change_seq().await.expect("journal head");
        assert_eq!(head, PINGERS * SYNC_ITERS);
        // Busy_timeout is 5s; if writers serialized into real lock waits the
        // whole run (1600 writes + 640 reads on WAL) lands in seconds, not
        // tens of seconds. Generous bound to stay true on loaded CI runners.
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "concurrent sync-state writes serialized into ping latency spikes: {:?}",
            elapsed
        );
    }
}
