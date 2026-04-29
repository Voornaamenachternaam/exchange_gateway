// src/storage.rs
use anyhow::{anyhow, Result};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, OptionalExtension};
use rusqlite_from_row::FromRow;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

pub type SqlPool = Pool<SqliteConnectionManager>;

#[derive(Clone)]
pub struct Storage {
    pool: Arc<SqlPool>,
}

impl Storage {
    pub fn pool(&self) -> Arc<SqlPool> {
        self.pool.clone()
    }
}

#[derive(FromRow, Debug)]
pub struct JournalRow {
    pub seq: i64,
    pub server_id: String,
    pub op: String,
    pub resource_href: Option<String>,
}

#[derive(FromRow, Debug)]
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

#[derive(FromRow, Debug, Deserialize)]
pub struct CalendarExceptionRow {
    pub parent_server_id: String,
    pub exception_start: String,
    pub server_id: Option<String>,
    pub is_deleted: i32,
    pub created_at: String,
}

#[derive(FromRow, Debug, Deserialize)]
pub struct MeetingResponseRow {
    pub request_id: String,
    pub calendar_id: String,
    pub user_response: i32,
    pub created_at: String,
}

#[derive(FromRow, Debug, Deserialize)]
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

#[derive(FromRow, Debug, Deserialize)]
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

#[derive(FromRow, Debug, Deserialize)]
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
    pub fn new(db_path: &str) -> Result<Self> {
        let manager = SqliteConnectionManager::file(db_path);
        let pool = Pool::builder()
            .max_size(16)
            .build(manager)
            .map_err(|e| anyhow!("Failed to create SQLite pool: {}", e))?;
        Ok(Self {
            pool: Arc::new(pool),
        })
    }

    pub fn init_schema(&self) -> Result<()> {
        let pool = self.pool.clone();
        tokio::task::block_in_place(|| {
            let conn = pool.get().map_err(|e| anyhow!("Pool get error: {}", e))?;
            conn.execute_batch(include_str!("../d1_schema.sql"))
                .map_err(|e| anyhow!("Schema init error: {}", e))
        });
        Ok(())
    }

    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let owner = owner.to_string();
        let collection_id = collection_id.to_string();
        let sync_key = sync_key.to_string();
        let token = token.map(|s| s.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO sync_state (owner, collection_id, sync_key, token) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(owner, collection_id) DO UPDATE SET sync_key = ?3, token = ?4, updated_at = CURRENT_TIMESTAMP",
                params![owner, collection_id, sync_key, token],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        let pool = self.pool.clone();
        let owner = owner.to_string();
        let collection_id = collection_id.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT sync_key, token FROM sync_state WHERE owner = ?1 AND collection_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![owner, collection_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            }).optional().map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
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
        let pool = self.pool.clone();
        let params = (
            owner.to_string(), caldav_href.to_string(), resource_href.to_string(),
            server_id.to_string(), uid.to_string(), etag.to_string()
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let tx = conn.unchecked_transaction();
            let result = (|| {
                tx.execute(
                    "INSERT INTO item_map (owner, caldav_href, resource_href, server_id, uid, etag) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(owner, server_id) DO UPDATE SET caldav_href = ?2, resource_href = ?3, uid = ?5, etag = ?6, updated_at = CURRENT_TIMESTAMP",
                    params![params.0, params.1, params.2, params.3, params.4, params.5],
                ).map_err(|e| anyhow!("DB error: {}", e))?;
                tx.execute(
                    "INSERT INTO change_journal (owner, server_id, op, resource_href) VALUES (?1, ?2, 'upsert', ?3)",
                    params![params.0, params.3, params.2],
                ).map_err(|e| anyhow!("DB error: {}", e))?;
                Ok::<(), rusqlite::Error>(())
            })();
            match result {
                Ok(()) => tx.commit().map_err(|e| anyhow!("Commit error: {}", e)),
                Err(e) => {
                    tx.rollback().ok();
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), collection_id.to_string(), client_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT server_id, status FROM client_sync_command WHERE owner = ?1 AND collection_id = ?2 AND client_id = ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1, params.2], |row| {
                Ok((row.get(0)?, row.get(1)?))
            }).optional().map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn put_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
        server_id: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (
            owner.to_string(), collection_id.to_string(), client_id.to_string(),
            server_id.map(|s| s.to_string()), status.to_string()
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO client_sync_command (owner, collection_id, client_id, server_id, status) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(owner, collection_id, client_id) DO UPDATE SET server_id = ?4, status = ?5, updated_at = CURRENT_TIMESTAMP",
                params![params.0, params.1, params.2, params.3, params.4],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), server_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM item_map WHERE owner = ?1 AND server_id = ?2",
                params![params.0, params.1],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), server_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let tx = conn.unchecked_transaction();
            let result = (|| {
                tx.execute(
                    "INSERT OR REPLACE INTO deleted_item_tombstone (owner, server_id) VALUES (?1, ?2)",
                    params![params.0, params.1],
                ).map_err(|e| anyhow!("DB error: {}", e))?;
                tx.execute(
                    "INSERT INTO change_journal (owner, server_id, op) VALUES (?1, ?2, 'delete')",
                    params![params.0, params.1],
                ).map_err(|e| anyhow!("DB error: {}", e))?;
                Ok::<(), rusqlite::Error>(())
            })();
            match result {
                Ok(()) => tx.commit().map_err(|e| anyhow!("Commit error: {}", e)),
                Err(e) => {
                    tx.rollback().ok();
                    Err(e)
                }
            }
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_changes_since(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        limit: i64,
    ) -> Result<Vec<EwsItemRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), format!("%{}%", collection_id), sync_key.to_string(), limit);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT im.server_id, im.resource_href, im.uid, im.etag, im.updated_at
                 FROM item_map im
                 WHERE im.owner = ?1 AND im.resource_href LIKE ?2
                 ORDER BY im.updated_at DESC LIMIT ?4",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1, params.2, params.3], EwsItemRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_deleted_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<String>> {
        let pool = self.pool.clone();
        let owner = owner.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT server_id FROM deleted_item_tombstone WHERE owner = ?1 AND deleted_at > datetime(?2, 'unixepoch')",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![owner, since_unix_ts], |row| row.get(0))
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        let pool = self.pool.clone();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.query_row("SELECT COALESCE(MAX(id), 0) FROM change_journal", [], |row| row.get(0))
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since: i64,
        limit: i64,
    ) -> Result<Vec<EwsItemRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), since, limit);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT im.server_id, im.resource_href, im.uid, im.etag, im.updated_at
                 FROM item_map im
                 WHERE im.owner = ?1 AND im.server_id IN (
                     SELECT server_id FROM change_journal WHERE owner = ?1 AND id > ?2 AND op = 'upsert'
                 )
                 ORDER BY im.updated_at DESC LIMIT ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1, params.2], EwsItemRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_deleted_since_seq(&self, owner: &str, since: i64) -> Result<Vec<(i64, String)>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), since);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, server_id FROM change_journal WHERE owner = ?1 AND id > ?2 AND op = 'delete'",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_journal_since_seq(&self, owner: &str, since: i64) -> Result<Vec<JournalRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), since);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, server_id, op, resource_href FROM change_journal WHERE owner = ?1 AND id > ?2 ORDER BY id ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], JournalRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn set_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
        policy_key: &str,
        policy_status: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), device_id.to_string(), policy_key.to_string(), policy_status.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO provision_state (owner, device_id, policy_key, policy_status) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(owner, device_id) DO UPDATE SET policy_key = ?3, policy_status = ?4, updated_at = CURRENT_TIMESTAMP",
                params![params.0, params.1, params.2, params.3],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<(String, String)>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), device_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT policy_key, policy_status FROM provision_state WHERE owner = ?1 AND device_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_device_info(&self, params: &DeviceInfoParams<'_>) -> Result<()> {
        let pool = self.pool.clone();
        let p = (
            params.owner.to_string(), params.device_id.to_string(),
            params.friendly_name.to_string(), params.model.to_string(),
            params.os.to_string(), params.phone_number.to_string(),
            params.imei.to_string(), params.user_agent.to_string()
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO device_info (user_email, device_id, friendly_name, model, os, phone_number, imei, user_agent)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT(user_email, device_id) DO UPDATE SET
                 friendly_name = ?3, model = ?4, os = ?5, phone_number = ?6, imei = ?7, user_agent = ?8, last_seen = CURRENT_TIMESTAMP",
                params![p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn list_ews_items(&self, owner: &str, limit: i64, offset: i64) -> Result<Vec<EwsItemRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), limit, offset);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1, params.2], EwsItemRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_ews_sync_state(&self, owner: &str, folder_id: &str) -> Result<Option<String>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), folder_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT sync_state FROM ews_sync_state WHERE user_email = ?1 AND folder_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], |row| row.get(0)).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), server_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner = ?1 AND server_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], EwsItemRow::from_row).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_ews_item_owner(&self, server_id: &str) -> Result<Option<String>> {
        let pool = self.pool.clone();
        let server_id = server_id.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare("SELECT owner FROM item_map WHERE server_id = ?1").map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![server_id], |row| row.get(0)).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn set_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
        sync_state: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), folder_id.to_string(), sync_state.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO ews_sync_state (user_email, folder_id, sync_state) VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_email, folder_id) DO UPDATE SET sync_state = ?3",
                params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
        server_id: Option<&str>,
        is_deleted: bool,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (
            owner.to_string(), parent_server_id.to_string(), exception_start.to_string(),
            server_id.map(|s| s.to_string()), if is_deleted { 1i32 } else { 0i32 }
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO calendar_exceptions (owner, parent_server_id, exception_start, server_id, is_deleted) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(owner, parent_server_id, exception_start) DO UPDATE SET server_id = ?4, is_deleted = ?5",
                params![params.0, params.1, params.2, params.3, params.4],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_calendar_exceptions(&self, owner: &str, parent_server_id: &str) -> Result<Vec<CalendarExceptionRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), parent_server_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], CalendarExceptionRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<Option<CalendarExceptionRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), parent_server_id.to_string(), exception_start.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2 AND exception_start = ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1, params.2], CalendarExceptionRow::from_row).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), parent_server_id.to_string(), exception_start.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM calendar_exceptions WHERE owner = ?1 AND parent_server_id = ?2 AND exception_start = ?3",
                params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn record_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
        calendar_id: &str,
        user_response: i32,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), request_id.to_string(), calendar_id.to_string(), user_response);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO meeting_response (owner, request_id, calendar_id, user_response) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(owner, request_id) DO UPDATE SET calendar_id = ?3, user_response = ?4",
                params![params.0, params.1, params.2, params.3],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<MeetingResponseRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), request_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT request_id, calendar_id, user_response, created_at FROM meeting_response WHERE owner = ?1 AND request_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], MeetingResponseRow::from_row).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_meeting_state(&self, params: &MeetingStateParams<'_>) -> Result<()> {
        let pool = self.pool.clone();
        let p = (
            params.uid.to_string(), params.owner.to_string(), params.sequence,
            params.state.to_string(), params.state_flags, params.is_organizer as i32,
            params.organizer_email.map(|s| s.to_string()), params.organizer_name.map(|s| s.to_string()),
            params.subject.map(|s| s.to_string()), params.location.map(|s| s.to_string()),
            params.start_time.to_string(), params.end_time.to_string(),
            params.timezone.map(|s| s.to_string())
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO meeting_state (uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                 ON CONFLICT(owner, uid) DO UPDATE SET
                 sequence = ?3, state = ?4, state_flags = ?5, is_organizer = ?6, organizer_email = ?7, organizer_name = ?8,
                 subject = ?9, location = ?10, start_time = ?11, end_time = ?12, timezone = ?13,
                 last_sequence_time = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP",
                params![p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7, p.8, p.9, p.10, p.11, p.12],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_meeting_state(&self, owner: &str, uid: &str) -> Result<Option<MeetingStateRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), uid.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone, created_at, updated_at, last_sequence_time
                 FROM meeting_state WHERE owner = ?1 AND uid = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], MeetingStateRow::from_row).optional()
                .map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_meeting_state(&self, owner: &str, uid: &str) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), uid.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM meeting_state WHERE owner = ?1 AND uid = ?2",
                params![params.0, params.1],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_meeting_attendee(&self, params: &MeetingAttendeeParams<'_>) -> Result<()> {
        let pool = self.pool.clone();
        let p = (
            params.meeting_uid.to_string(), params.owner.to_string(), params.email.to_string(),
            params.name.map(|s| s.to_string()), params.status, params.role,
            params.response_time.map(|s| s.to_string()), params.proposed_start.map(|s| s.to_string()),
            params.proposed_end.map(|s| s.to_string()), params.sequence
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO meeting_attendee (meeting_uid, owner, email, name, status, role, response_time, proposed_start, proposed_end, sequence)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(owner, meeting_uid, email) DO UPDATE SET
                 name = ?4, status = ?5, role = ?6, response_time = ?7, proposed_start = ?8, proposed_end = ?9, sequence = ?10, updated_at = CURRENT_TIMESTAMP",
                params![p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7, p.8, p.9],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_meeting_attendees(&self, owner: &str, meeting_uid: &str) -> Result<Vec<MeetingAttendeeRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), meeting_uid.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT meeting_uid, owner, email, name, status, role, response_time, proposed_start, proposed_end, sequence, created_at, updated_at
                 FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2 ORDER BY email ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], MeetingAttendeeRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_meeting_attendee(
        &self,
        owner: &str,
        meeting_uid: &str,
        email: &str,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), meeting_uid.to_string(), email.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2 AND email = ?3",
                params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_meeting_attendees(&self, owner: &str, meeting_uid: &str) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), meeting_uid.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM meeting_attendee WHERE owner = ?1 AND meeting_uid = ?2",
                params![params.0, params.1],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn enqueue_scheduling(
        &self,
        owner: &str,
        meeting_uid: &str,
        operation: &str,
        sequence: u32,
        ical_data: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (
            meeting_uid.to_string(), owner.to_string(), operation.to_string(),
            sequence, ical_data.map(|s| s.to_string())
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO meeting_scheduling_queue (meeting_uid, owner, operation, sequence, ical_data, status, attempts) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', 0)",
                params![params.0, params.1, params.2, params.3, params.4],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_pending_scheduling(&self, owner: &str, limit: i64) -> Result<Vec<SchedulingQueueRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), limit.min(100));
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, meeting_uid, owner, operation, sequence, ical_data, status, attempts, last_attempt, error_message, created_at, processed_at
                 FROM meeting_scheduling_queue WHERE owner = ?1 AND status = 'pending' ORDER BY id ASC LIMIT ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], SchedulingQueueRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn mark_scheduling_processed(
        &self,
        id: i64,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let params = (status.to_string(), error_message.map(|s| s.to_string()), id);
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "UPDATE meeting_scheduling_queue SET status = ?1, error_message = ?2, attempts = attempts + 1, last_attempt = CURRENT_TIMESTAMP, processed_at = CURRENT_TIMESTAMP WHERE id = ?3",
                params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_meetings_by_time_range(
        &self,
        owner: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<MeetingStateRow>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), start.to_string(), end.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT uid, owner, sequence, state, state_flags, is_organizer, organizer_email, organizer_name, subject, location, start_time, end_time, timezone, created_at, updated_at, last_sequence_time
                 FROM meeting_state WHERE owner = ?1 AND start_time >= ?2 AND end_time <= ?3 ORDER BY start_time ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1, params.2], MeetingStateRow::from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_calendar_attachment(
        &self,
        attachment: &crate::attachment::AttachmentRecord,
    ) -> Result<()> {
        let pool = self.pool.clone();
        let p = (
            attachment.id.clone(), attachment.parent_item_server_id.clone(),
            attachment.owner.clone(), attachment.name.clone(),
            attachment.content_type.clone(), attachment.content_size,
            attachment.content_base64.clone(),
            if attachment.is_inline { 1i32 } else { 0i32 },
            attachment.content_id.clone(), attachment.content_location.clone(),
            attachment.attachment_type.clone(), attachment.last_modified_time.clone()
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO calendar_attachment (id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(id) DO UPDATE SET
                 parent_item_server_id = ?2, name = ?4, content_type = ?5, content_size = ?6, content_base64 = ?7, is_inline = ?8,
                 content_id = ?9, content_location = ?10, attachment_type = ?11, last_modified_time = ?12, updated_at = CURRENT_TIMESTAMP",
                params![p.0, p.1, p.2, p.3, p.4, p.5, p.6, p.7, p.8, p.9, p.10, p.11],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_calendar_attachment(
        &self,
        owner: &str,
        attachment_id: &str,
    ) -> Result<Option<crate::attachment::AttachmentRecord>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), attachment_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time
                 FROM calendar_attachment WHERE owner = ?1 AND id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(params![params.0, params.1], |row| {
                Ok(crate::attachment::AttachmentRecord {
                    id: row.get(0)?,
                    parent_item_server_id: row.get(1)?,
                    owner: row.get(2)?,
                    name: row.get(3)?,
                    content_type: row.get(4)?,
                    content_size: row.get(5)?,
                    content_base64: row.get(6)?,
                    is_inline: row.get::<_, i32>(7)? != 0,
                    content_id: row.get(8)?,
                    content_location: row.get(9)?,
                    attachment_type: row.get(10)?,
                    last_modified_time: row.get(11)?,
                })
            }).optional().map_err(|e| anyhow!("Query error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_calendar_attachments_for_item(
        &self,
        owner: &str,
        parent_item_server_id: &str,
    ) -> Result<Vec<crate::attachment::AttachmentRecord>> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), parent_item_server_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, parent_item_server_id, owner, name, content_type, content_size, content_base64, is_inline, content_id, content_location, attachment_type, last_modified_time
                 FROM calendar_attachment WHERE owner = ?1 AND parent_item_server_id = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![params.0, params.1], |row| {
                Ok(crate::attachment::AttachmentRecord {
                    id: row.get(0)?,
                    parent_item_server_id: row.get(1)?,
                    owner: row.get(2)?,
                    name: row.get(3)?,
                    content_type: row.get(4)?,
                    content_size: row.get(5)?,
                    content_base64: row.get(6)?,
                    is_inline: row.get::<_, i32>(7)? != 0,
                    content_id: row.get(8)?,
                    content_location: row.get(9)?,
                    attachment_type: row.get(10)?,
                    last_modified_time: row.get(11)?,
                })
            }).map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_calendar_attachment(&self, owner: &str, attachment_id: &str) -> Result<()> {
        let pool = self.pool.clone();
        let params = (owner.to_string(), attachment_id.to_string());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM calendar_attachment WHERE owner = ?1 AND id = ?2",
                params![params.0, params.1],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_room_list(&self, room_list: &crate::room::RoomListRecord) -> Result<()> {
        let pool = self.pool.clone();
        let params = (room_list.id.clone(), room_list.email.clone(), room_list.name.clone());
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO room_list (id, email, name) VALUES (?1, ?2, ?3)
                 ON CONFLICT(email) DO UPDATE SET name = ?3, updated_at = CURRENT_TIMESTAMP",
                params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_room_lists(&self, _owner: &str) -> Result<Vec<crate::room::RoomListRecord>> {
        let pool = self.pool.clone();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare("SELECT id, email, name FROM room_list ORDER BY name ASC")
                .map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map([], |row| {
                Ok(crate::room::RoomListRecord {
                    id: row.get(0)?,
                    email: row.get(1)?,
                    name: row.get(2)?,
                })
            }).map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_room_list(&self, _owner: &str, email: &str) -> Result<()> {
        let pool = self.pool.clone();
        let email = email.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute("DELETE FROM room_list WHERE email = ?1", params![email])
                .map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_all_rooms(&self, _owner: &str) -> Result<Vec<crate::room::RoomRecord>> {
        let pool = self.pool.clone();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare("SELECT id, room_list_email, email, name, capacity, is_available FROM room ORDER BY name ASC")
                .map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map([], |row| {
                Ok(crate::room::RoomRecord {
                    id: row.get(0)?,
                    room_list_email: row.get(1)?,
                    email: row.get(2)?,
                    name: row.get(3)?,
                    capacity: row.get(4)?,
                    is_available: row.get::<_, i32>(5)? != 0,
                })
            }).map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_room(&self, _owner: &str, email: &str) -> Result<()> {
        let pool = self.pool.clone();
        let email = email.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute("DELETE FROM room WHERE email = ?1", params![email])
                .map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_room(&self, room: &crate::room::RoomRecord) -> Result<()> {
        let pool = self.pool.clone();
        let params = (
            room.id.clone(), room.room_list_email.clone(), room.email.clone(),
            room.name.clone(), room.capacity, if room.is_available { 1i32 } else { 0i32 }
        );
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO room (id, room_list_email, email, name, capacity, is_available) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(email) DO UPDATE SET room_list_email = ?2, name = ?4, capacity = ?5, is_available = ?6, updated_at = CURRENT_TIMESTAMP",
                params![params.0, params.1, params.2, params.3, params.4, params.5],
            ).map_err(|e| anyhow!("DB error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_rooms_for_list(
        &self,
        _owner: &str,
        room_list_email: &str,
    ) -> Result<Vec<crate::room::RoomRecord>> {
        let pool = self.pool.clone();
        let room_list_email = room_list_email.to_string();
        
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, room_list_email, email, name, capacity, is_available FROM room WHERE room_list_email = ?1 ORDER BY name ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(params![room_list_email], |row| {
                Ok(crate::room::RoomRecord {
                    id: row.get(0)?,
                    room_list_email: row.get(1)?,
                    email: row.get(2)?,
                    name: row.get(3)?,
                    capacity: row.get(4)?,
                    is_available: row.get::<_, i32>(5)? != 0,
                })
            }).map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }
}