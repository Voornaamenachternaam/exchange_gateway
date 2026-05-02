// src/permission/storage.rs
use crate::permission::types::{CalendarPermission, DelegateInfo, PermissionAuditEntry};
use crate::storage::Storage;
use crate::storage::SafeDebug;
use anyhow::{Result, anyhow};
use sqlx::FromRow;

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

// Internal row struct - converted to CalendarPermission for public use
#[derive(FromRow)]
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

impl SafeDebug for PermissionRow {
    fn safe_debug(&self) -> String {
        format!(
            "PermissionRow {{ id: {:?}, folder_id: {:?}, owner: {:?}, user_email: {:?}, user_name: {:?}, rights: {}, is_default: {}, is_anonymous: {} }}",
            self.id, self.folder_id, self.owner, self.user_email, self.user_name, self.rights, self.is_default, self.is_anonymous
        )
    }
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

// Internal row struct - converted to DelegateInfo for public use
#[derive(FromRow)]
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

impl SafeDebug for DelegateRow {
    fn safe_debug(&self) -> String {
        format!(
            "DelegateRow {{ id: {:?}, delegator: {:?}, delegate_email: {:?}, delegate_name: {:?}, calendar_permission: {}, inbox_permission: {}, tasks_permission: {}, contacts_permission: {}, notes_permission: {}, journal_permission: {}, receive_copies: {}, receive_infos: {}, view_private: {} }}",
            self.id, self.delegator, self.delegate_email, self.delegate_name,
            self.calendar_permission, self.inbox_permission, self.tasks_permission,
            self.contacts_permission, self.notes_permission, self.journal_permission,
            self.receive_copies, self.receive_infos, self.view_private
        )
    }
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

// Internal row struct - converted to PermissionAuditEntry for public use
#[derive(FromRow)]
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

impl SafeDebug for AuditRow {
    fn safe_debug(&self) -> String {
        format!(
            "AuditRow {{ id: {:?}, folder_id: {:?}, owner: {:?}, actor_email: {:?}, target_email: {:?}, operation: {:?}, old_rights: {:?}, new_rights: {:?} }}",
            self.id, self.folder_id, self.owner, self.actor_email, self.target_email,
            self.operation, self.old_rights, self.new_rights
        )
    }
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
        let row = sqlx::query_as::<_, PermissionRow>(
            "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND user_email = ?3"
        )
        .bind(owner)
        .bind(folder_id)
        .bind(user_email)
        .fetch_optional(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_permissions_for_folder(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let rows = sqlx::query_as::<_, PermissionRow>(
            "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 ORDER BY user_email ASC"
        )
        .bind(owner)
        .bind(folder_id)
        .fetch_all(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn get_permissions_for_user(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let rows = sqlx::query_as::<_, PermissionRow>(
            "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND user_email = ?2 ORDER BY folder_id ASC"
        )
        .bind(owner)
        .bind(user_email)
        .fetch_all(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn upsert_permission(&self, permission: &CalendarPermission) -> Result<()> {
        sqlx::query(
            "INSERT INTO calendar_permission (id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(id) DO UPDATE SET folder_id = ?2, user_name = ?5, rights = ?6, is_default = ?7, is_anonymous = ?8, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(&permission.id)
        .bind(&permission.folder_id)
        .bind(&permission.owner)
        .bind(&permission.user_email)
        .bind(&permission.user_name)
        .bind(permission.rights as i32)
        .bind(if permission.is_default { 1i32 } else { 0i32 })
        .bind(if permission.is_anonymous { 1i32 } else { 0i32 })
        .execute(self.storage.pool())
        .await
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn delete_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<()> {
        sqlx::query("DELETE FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND user_email = ?3")
            .bind(owner)
            .bind(folder_id)
            .bind(user_email)
            .execute(self.storage.pool())
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn get_default_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let row = sqlx::query_as::<_, PermissionRow>(
            "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND is_default = 1"
        )
        .bind(owner)
        .bind(folder_id)
        .fetch_optional(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_anonymous_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let row = sqlx::query_as::<_, PermissionRow>(
            "SELECT id, folder_id, owner, user_email, user_name, rights, is_default, is_anonymous, created_at, updated_at FROM calendar_permission WHERE owner = ?1 AND folder_id = ?2 AND is_anonymous = 1"
        )
        .bind(owner)
        .bind(folder_id)
        .fetch_optional(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateInfo>> {
        let row = sqlx::query_as::<_, DelegateRow>(
            "SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ?1 AND delegate_email = ?2"
        )
        .bind(delegator)
        .bind(delegate_email)
        .fetch_optional(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(row.map(DelegateInfo::from))
    }

    pub async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let rows = sqlx::query_as::<_, DelegateRow>(
            "SELECT id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private, created_at, updated_at FROM calendar_delegate WHERE delegator = ?1 ORDER BY delegate_email ASC"
        )
        .bind(delegator)
        .fetch_all(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(rows.into_iter().map(DelegateInfo::from).collect())
    }

    pub async fn upsert_delegate(&self, delegate: &DelegateInfo) -> Result<()> {
        sqlx::query(
            "INSERT INTO calendar_delegate (id, delegator, delegate_email, delegate_name, calendar_permission, inbox_permission, tasks_permission, contacts_permission, notes_permission, journal_permission, receive_copies, receive_infos, view_private) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) ON CONFLICT(delegator, delegate_email) DO UPDATE SET delegate_name = ?4, calendar_permission = ?5, inbox_permission = ?6, tasks_permission = ?7, contacts_permission = ?8, notes_permission = ?9, journal_permission = ?10, receive_copies = ?11, receive_infos = ?12, view_private = ?13, updated_at = CURRENT_TIMESTAMP"
        )
        .bind(&delegate.id)
        .bind(&delegate.delegator)
        .bind(&delegate.delegate_email)
        .bind(&delegate.delegate_name)
        .bind(delegate.calendar_permission as i32)
        .bind(delegate.inbox_permission as i32)
        .bind(delegate.tasks_permission as i32)
        .bind(delegate.contacts_permission as i32)
        .bind(delegate.notes_permission as i32)
        .bind(delegate.journal_permission as i32)
        .bind(if delegate.receive_copies { 1i32 } else { 0i32 })
        .bind(if delegate.receive_infos { 1i32 } else { 0i32 })
        .bind(if delegate.view_private { 1i32 } else { 0i32 })
        .execute(self.storage.pool())
        .await
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn delete_delegate(&self, delegator: &str, delegate_email: &str) -> Result<()> {
        sqlx::query("DELETE FROM calendar_delegate WHERE delegator = ?1 AND delegate_email = ?2")
            .bind(delegator)
            .bind(delegate_email)
            .execute(self.storage.pool())
            .await
            .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn add_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO permission_audit (id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        )
        .bind(&entry.id)
        .bind(&entry.folder_id)
        .bind(&entry.owner)
        .bind(&entry.actor_email)
        .bind(&entry.target_email)
        .bind(&entry.operation)
        .bind(entry.old_rights.map(|v| v as i32))
        .bind(entry.new_rights.map(|v| v as i32))
        .execute(self.storage.pool())
        .await
        .map_err(|e| anyhow!("DB error: {}", e))?;
        Ok(())
    }

    pub async fn get_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<PermissionAuditEntry>> {
        let rows = sqlx::query_as::<_, AuditRow>(
            "SELECT id, folder_id, owner, actor_email, target_email, operation, old_rights, new_rights, created_at FROM permission_audit WHERE owner = ?1 AND folder_id = ?2 ORDER BY created_at DESC LIMIT ?3"
        )
        .bind(owner)
        .bind(folder_id)
        .bind(limit as i64)
        .fetch_all(self.storage.pool())
        .await
        .map_err(|e| anyhow!("Query error: {}", e))?;

        Ok(rows.into_iter().map(PermissionAuditEntry::from).collect())
    }
}
