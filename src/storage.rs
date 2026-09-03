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
#[derive(FromRow)]
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
#[derive(FromRow)]
pub struct NoteRow {
    pub id: i64,
    pub owner: String,
    pub server_id: String,
    pub subject: Option<String>,
    pub message_class: Option<String>,
    pub body: Option<String>,
    pub categories: Option<String>,
    pub last_modified_date: Option<String>,
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
    pub async fn new(database_url: &str) -> Result<Self> {
        let pool = Pool::connect(database_url).await.map_err(|e| {
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
            "INSERT INTO task_map (owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
             ON CONFLICT(owner, server_id) DO UPDATE SET \
             subject = excluded.subject, importance = excluded.importance, sensitivity = excluded.sensitivity, \
             start_date = excluded.start_date, due_date = excluded.due_date, utc_start_date = excluded.utc_start_date, \
             utc_due_date = excluded.utc_due_date, complete = excluded.complete, date_completed = excluded.date_completed, \
             reminder_set = excluded.reminder_set, reminder_time = excluded.reminder_time, categories = excluded.categories, \
             body = excluded.body, updated_at = CURRENT_TIMESTAMP"
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
            "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body, updated_at FROM task_map WHERE owner = ?1 AND server_id = ?2"
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
            "SELECT id, owner, server_id, subject, importance, sensitivity, start_date, due_date, utc_start_date, utc_due_date, complete, date_completed, reminder_set, reminder_time, categories, body, updated_at FROM task_map WHERE owner = ?1 ORDER BY server_id ASC"
        )
        .bind(owner)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
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
            "INSERT INTO note_map (owner, server_id, subject, message_class, body, categories, last_modified_date) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(owner, server_id) DO UPDATE SET \
             subject = excluded.subject, message_class = excluded.message_class, body = excluded.body, \
             categories = excluded.categories, last_modified_date = excluded.last_modified_date, \
             updated_at = CURRENT_TIMESTAMP"
        )
        .bind(owner)
        .bind(server_id)
        .bind(fields.subject)
        .bind(fields.message_class)
        .bind(fields.body)
        .bind(fields.categories)
        .bind(fields.last_modified_date)
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
            "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, updated_at FROM note_map WHERE owner = ?1 AND server_id = ?2"
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
            "SELECT id, owner, server_id, subject, message_class, body, categories, last_modified_date, updated_at FROM note_map WHERE owner = ?1 ORDER BY server_id ASC"
        )
        .bind(owner)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| GatewayError::Storage(format!("Query error: {}", e)))?;
        Ok(rows)
    }
}

/// Column values for a gateway-local task upsert.
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
}

/// Column values for a gateway-local note upsert.
#[derive(Default)]
pub struct NoteFields<'a> {
    pub subject: Option<&'a str>,
    pub message_class: Option<&'a str>,
    pub body: Option<&'a str>,
    pub categories: Option<&'a str>,
    pub last_modified_date: Option<&'a str>,
}
