// src/permission/storage.rs
use crate::permission::types::{CalendarPermission, DelegateInfo, PermissionAuditEntry};
use crate::storage::Storage;
use anyhow::{Result, anyhow};
use rusqlite::OptionalExtension;
use serde_rusqlite::from_row;

fn convert_row<T: serde::de::DeserializeOwned>(row: &rusqlite::Row) -> rusqlite::Result<T> {
    from_row(row).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn parse_sqlite_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .map(|dt| chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc))
        })
        .unwrap_or_else(|e| {
            tracing::warn!(timestamp = %s, error = %e, "Failed to parse timestamp, using current time");
            chrono::Utc::now()
        })
}

#[derive(Debug, Deserialize)]
struct PermissionRow {
    id: String,
    folder_id: String,
    owner: String,
    user_email: String,
    user_name: Option<String>,
    rights: i32,
    is_default: i32,
    is_anonymous: i32,
    created_at: String,
    updated_at: String,
}

impl From<PermissionRow> for CalendarPermission {
    fn from(row: PermissionRow) -> Self {
        Self {
            id: row.id,
            folder_id: row.folder_id,
            owner: row.owner,
            user_email: row.user_email,
            user_name: row.user_name,
            rights: row.rights.max(0) as u32,
            is_default: row.is_default != 0,
            is_anonymous: row.is_anonymous != 0,
            created_at: parse_sqlite_timestamp(&row.created_at),
            updated_at: parse_sqlite_timestamp(&row.updated_at),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DelegateRow {
    id: String,
    delegator: String,
    delegate_email: String,
    delegate_name: Option<String>,
    calendar_permission: i32,
    inbox_permission: i32,
    tasks_permission: i32,
    contacts_permission: i32,
    notes_permission: i32,
    journal_permission: i32,
    receive_copies: i32,
    receive_infos: i32,
    view_private: i32,
    created_at: String,
    updated_at: String,
}

impl From<DelegateRow> for DelegateInfo {
    fn from(row: DelegateRow) -> Self {
        Self {
            id: row.id,
            delegator: row.delegator,
            delegate_email: row.delegate_email,
            delegate_name: row.delegate_name,
            calendar_permission: row.calendar_permission as u8,
            inbox_permission: row.inbox_permission as u8,
            tasks_permission: row.tasks_permission as u8,
            contacts_permission: row.contacts_permission as u8,
            notes_permission: row.notes_permission as u8,
            journal_permission: row.journal_permission as u8,
            receive_copies: row.receive_copies != 0,
            receive_infos: row.receive_infos != 0,
            view_private: row.view_private != 0,
            created_at: parse_sqlite_timestamp(&row.created_at),
            updated_at: parse_sqlite_timestamp(&row.updated_at),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AuditRow {
    id: String,
    folder_id: String,
    owner: String,
    actor_email: String,
    target_email: String,
    operation: String,
    old_rights: Option<i32>,
    new_rights: Option<i32>,
    created_at: String,
}

impl From<AuditRow> for PermissionAuditEntry {
    fn from(row: AuditRow) -> Self {
        Self {
            id: row.id,
            folder_id: row.folder_id,
            owner: row.owner,
            actor_email: row.actor_email,
            target_email: row.target_email,
            operation: row.operation,
            old_rights: row.old_rights.filter(|&v| v >= 0).map(|v| v as u32),
            new_rights: row.new_rights.filter(|&v| v >= 0).map(|v| v as u32),
            created_at: parse_sqlite_timestamp(&row.created_at),
        }
    }
}

// Type-specific row conversion helpers
fn permission_row_from_row(row: &rusqlite::Row) -> rusqlite::Result<PermissionRow> {
    convert_row(row)
}

fn delegate_row_from_row(row: &rusqlite::Row) -> rusqlite::Result<DelegateRow> {
    convert_row(row)
}

fn audit_row_from_row(row: &rusqlite::Row) -> rusqlite::Result<AuditRow> {
    convert_row(row)
}

pub struct PermissionStorage<'a> {
    storage: &'a Storage,
}

impl<'a> PermissionStorage<'a> {
    pub fn new(storage: &'a Storage) -> Self {
        Self { storage }
    }

    pub async fn get_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<Option<CalendarPermission>> {
        let pool = self.storage.pool();
        let params = (
            owner.to_string(),
            folder_id.to_string(),
            user_email.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND user_email = ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(rusqlite::params![params.0, params.1, params.2], permission_row_from_row)
                .optional()
                .map_err(|e| anyhow!("Query error: {}", e))
                .map(|opt| opt.map(CalendarPermission::from))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_permissions_for_folder(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let pool = self.storage.pool();
        let params = (owner.to_string(), folder_id.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 ORDER BY user_email ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(rusqlite::params![params.0, params.1], permission_row_from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
                .map(|rows| rows.into_iter().map(CalendarPermission::from).collect())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_permissions_for_user(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let pool = self.storage.pool();
        let params = (owner.to_string(), user_email.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND user_email = ?2 ORDER BY folder_id ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(rusqlite::params![params.0, params.1], permission_row_from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
                .map(|rows| rows.into_iter().map(CalendarPermission::from).collect())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_permission(&self, permission: &CalendarPermission) -> Result<()> {
        let pool = self.storage.pool();
        let params = (
            permission.id.clone(),
            permission.folder_id.clone(),
            permission.owner.clone(),
            permission.user_email.clone(),
            permission.user_name.clone(),
            permission.rights as i32,
            if permission.is_default { 1i32 } else { 0i32 },
            if permission.is_anonymous { 1i32 } else { 0i32 },
        );
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO calendar_permission (id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO UPDATE SET folder_id = ?2, user_name = ?5, rights = ?6, is_default = ?7, is_anonymous = ?8, updated_at = CURRENT_TIMESTAMP",
                rusqlite::params![params.0, params.1, params.2, params.3, params.4, params.5, params.6, params.7],
            ).map_err(|e| anyhow!("DB error: {}", e))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<()> {
        let pool = self.storage.pool();
        let params = (
            owner.to_string(),
            folder_id.to_string(),
            user_email.to_string(),
        );
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND user_email = ?3",
                rusqlite::params![params.0, params.1, params.2],
            ).map_err(|e| anyhow!("DB error: {}", e))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_default_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let pool = self.storage.pool();
        let params = (owner.to_string(), folder_id.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND is_default = 1",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(rusqlite::params![params.0, params.1], permission_row_from_row)
                .optional()
                .map_err(|e| anyhow!("Query error: {}", e))
                .map(|opt| opt.map(CalendarPermission::from))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_anonymous_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let pool = self.storage.pool();
        let params = (owner.to_string(), folder_id.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND is_anonymous = 1",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(rusqlite::params![params.0, params.1], permission_row_from_row)
                .optional()
                .map_err(|e| anyhow!("Query error: {}", e))
                .map(|opt| opt.map(CalendarPermission::from))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateInfo>> {
        let pool = self.storage.pool();
        let params = (delegator.to_string(), delegate_email.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ?1 AND delegate_email = ?2",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_row(rusqlite::params![params.0, params.1], permission_row_from_row)
                .optional()
                .map_err(|e| anyhow!("Query error: {}", e))
                .map(|opt| opt.map(DelegateInfo::from))
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let pool = self.storage.pool();
        let delegator = delegator.to_string();
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ?1 ORDER BY delegate_email ASC",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(rusqlite::params![delegator], delegate_row_from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
                .map(|rows| rows.into_iter().map(DelegateInfo::from).collect())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn upsert_delegate(&self, delegate: &DelegateInfo) -> Result<()> {
        let pool = self.storage.pool();
        let params = (
            delegate.id.clone(),
            delegate.delegator.clone(),
            delegate.delegate_email.clone(),
            delegate.delegate_name.clone(),
            delegate.calendar_permission as i32,
            delegate.inbox_permission as i32,
            delegate.tasks_permission as i32,
            delegate.contacts_permission as i32,
            delegate.notes_permission as i32,
            delegate.journal_permission as i32,
            if delegate.receive_copies { 1i32 } else { 0i32 },
            if delegate.receive_infos { 1i32 } else { 0i32 },
            if delegate.view_private { 1i32 } else { 0i32 },
        );
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO calendar_delegate (id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT(delegator, delegate_email) DO UPDATE SET delegate_name = ?4, calendar_permission = ?5, inbox_permission = ?6, tasks_permission = ?7, contacts_permission = ?8, notes_permission = ?9, journal_permission = ?10, receive_copies = ?11, receive_infos = ?12, view_private = ?13, updated_at = CURRENT_TIMESTAMP",
                rusqlite::params![params.0, params.1, params.2, params.3, params.4, params.5, params.6, params.7, params.8, params.9, params.10, params.11, params.12],
            ).map_err(|e| anyhow!("DB error: {}", e))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn delete_delegate(&self, delegator: &str, delegate_email: &str) -> Result<()> {
        let pool = self.storage.pool();
        let params = (delegator.to_string(), delegate_email.to_string());
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "DELETE FROM calendar_delegate WHERE delegator = ?1 AND delegate_email = ?2",
                rusqlite::params![params.0, params.1],
            )
            .map_err(|e| anyhow!("DB error: {}", e))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn add_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        let pool = self.storage.pool();
        let params = (
            entry.id.clone(),
            entry.folder_id.clone(),
            entry.owner.clone(),
            entry.actor_email.clone(),
            entry.target_email.clone(),
            entry.operation.clone(),
            entry.old_rights.map(|v| v as i32),
            entry.new_rights.map(|v| v as i32),
        );
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            conn.execute(
                "INSERT INTO permission_audit (id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![params.0, params.1, params.2, params.3, params.4, params.5, params.6, params.7],
            ).map_err(|e| anyhow!("DB error: {}", e))?;
            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }

    pub async fn get_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<PermissionAuditEntry>> {
        let pool = self.storage.pool();
        let params = (owner.to_string(), folder_id.to_string(), limit as i64);
        tokio::task::spawn_blocking(move || {
            let conn = pool.get().map_err(|e| anyhow!("Pool error: {}", e))?;
            let mut stmt = conn.prepare(
                "SELECT id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights, created_at FROM permission_audit WHERE owner = ?1 AND folder_id = ?2 ORDER BY created_at DESC LIMIT ?3",
            ).map_err(|e| anyhow!("Prepare error: {}", e))?;
            stmt.query_map(rusqlite::params![params.0, params.1, params.2], audit_row_from_row)
                .map_err(|e| anyhow!("Query map error: {}", e))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| anyhow!("Collect error: {}", e))
                .map(|rows| rows.into_iter().map(PermissionAuditEntry::from).collect())
        })
        .await
        .map_err(|e| anyhow!("Task join error: {}", e))?
    }
}
