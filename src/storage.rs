// src/storage.rs
use sqlx::{Executor, FromRow, Pool, Row, Sqlite};
use std::sync::Arc;

use crate::error::{GatewayError, Result};

pub type SqlPool = Pool<Sqlite>;

#[derive(Clone)]
pub struct Storage {
    pool: Arc<SqlPool>,
}

impl Storage {
    pub fn pool(&self) -> &SqlPool {
        &self.pool
    }
}

#[derive(Debug, FromRow)]
pub struct JournalRow {
    pub id: i64,
    pub server_id: String,
    pub op: String,
    pub resource_href: Option<String>,
}

#[derive(Debug, FromRow)]
pub struct EwsItemRow {
    pub server_id: String,
    pub resource_href: String,
    pub uid: Option<String>,
    pub etag: Option<String>,
    pub updated_at: Option<String>,
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

#[derive(Debug, FromRow)]
pub struct CalendarExceptionRow {
    pub parent_server_id: String,
    pub exception_start: String,
    pub server_id: Option<String>,
    pub is_deleted: i32,
    pub created_at: String,
}

#[derive(Debug, FromRow)]
pub struct MeetingResponseRow {
    pub request_id: String,
    pub calendar_id: String,
    pub user_response: i32,
    pub created_at: String,
}

#[derive(Debug, FromRow)]
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

#[derive(Debug, FromRow)]
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

#[derive(Debug, FromRow)]
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
            .map_err(|e| anyhow!("Transaction error: {}", e))?;

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
        .map_err(|e| anyhow!("DB error: {}", e))?;

        sqlx::query(
            "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'upsert', ?3)"
        )
        .bind(owner)
        .bind(server_id)
        .bind(resource_href)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("DB error: {}", e))?;

        tx.commit()
            .await
            .map_err(|e| anyhow!("Commit error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        sqlx::query("DELETE FROM item_map WHERE owner = ?1 AND server_id = ?2")
            .bind(owner)
            .bind(server_id)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        let mut tx = sqlx::Acquire::begin(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Transaction error: {}", e))?;

        sqlx::query(
            "INSERT OR REPLACE INTO deleted_item_tombstone (owner, server_id) VALUES (?1, ?2)",
        )
        .bind(owner)
        .bind(server_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| anyhow!("DB error: {}", e))?;

        sqlx::query("INSERT INTO change_journal (owner, server_id, op) VALUES (?1, ?2, 'delete')")
            .bind(owner)
            .bind(server_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;

        tx.commit()
            .await
            .map_err(|e| anyhow!("Commit error: {}", e))?;
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
            "SELECT im.server_id, im.resource_href, im.uid, im.etag, im.updated_at
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
        .map_err(|e| anyhow!("Query error: {}", e))
    }

    pub async fn list_deleted_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<String>> {
        let rows = sqlx::query("SELECT server_id FROM deleted_item_tombstone WHERE owner = ?1 AND deleted_at > datetime(?2, 'unixepoch')")
            .bind(owner)
            .bind(since_unix_ts)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(rows.into_iter().map(|r| r.get(0)).collect())
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        let row = sqlx::query("SELECT COALESCE(MAX(id), 0) FROM change_journal")
            .fetch_one(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;
        Ok(row.get(0))
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since: i64,
        limit: i64,
    ) -> Result<Vec<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT im.server_id, im.resource_href, im.uid, im.etag, im.updated_at
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
        .map_err(|e| anyhow!("Query error: {}", e))
    }

    pub async fn list_deleted_since_seq(
        &self,
        owner: &str,
        since: i64,
    ) -> Result<Vec<(i64, String)>> {
        let rows = sqlx::query("SELECT id, server_id FROM change_journal WHERE owner = ?1 AND id > ?2 AND op = 'delete' ORDER BY id ASC")
            .bind(owner)
            .bind(since)
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;

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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn list_ews_items(
        &self,
        owner: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 ORDER BY updated_at DESC, server_id ASC LIMIT ?2 OFFSET ?3"
        )
        .bind(owner)
        .bind(limit)
        .bind(offset)
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))
    }

    pub async fn get_ews_sync_state(&self, owner: &str, folder_id: &str) -> Result<Option<String>> {
        let row = sqlx::query(
            "SELECT sync_state FROM ews_sync_state WHERE user_email = ?1 AND folder_id = ?2",
        )
        .bind(owner)
        .bind(folder_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(row.map(|r| r.get(0)))
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        sqlx::query_as::<_, EwsItemRow>(
            "SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 AND server_id = ?2"
        )
        .bind(owner)
        .bind(server_id)
        .fetch_optional(self.pool.as_ref())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))
    }

    pub async fn verify_item_owner(&self, owner: &str, server_id: &str) -> Result<bool> {
        let row = sqlx::query("SELECT 1 FROM item_map WHERE owner = ?1 AND server_id = ?2 LIMIT 1")
            .bind(owner)
            .bind(server_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;
        Ok(row.is_some())
    }

    pub async fn get_item_owner(&self, server_id: &str) -> Result<Option<String>> {
        let row = sqlx::query("SELECT owner FROM item_map WHERE server_id = ?1")
            .bind(server_id)
            .fetch_optional(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
            .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
    }

    pub async fn delete_meeting_state(&self, owner: &str, uid: &str) -> Result<()> {
        sqlx::query("DELETE FROM meeting_state WHERE owner = ?1 AND uid = ?2")
            .bind(owner)
            .bind(uid)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn delete_meeting_attendees(&self, owner: &str, meeting_uid: &str) -> Result<()> {
        sqlx::query("DELETE FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2")
            .bind(owner)
            .bind(meeting_uid)
            .execute(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
            .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn get_room_lists(&self, _owner: &str) -> Result<Vec<crate::room::RoomListRecord>> {
        let rows = sqlx::query(
            "SELECT id, email, name, created_at, updated_at FROM room_list ORDER BY name ASC",
        )
        .fetch_all(self.pool.as_ref())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
            .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn get_all_rooms(&self, _owner: &str) -> Result<Vec<crate::room::RoomRecord>> {
        let rows = sqlx::query("SELECT id, room_list_email, email, name, capacity, is_available, created_at, updated_at FROM room ORDER BY name ASC")
            .fetch_all(self.pool.as_ref())
            .await
            .map_err(|e| anyhow!("Query error: {}", e))?;

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
            .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("DB error: {}", e))?;
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
        .map_err(|e| anyhow!("Query error: {}", e))?;

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
}
