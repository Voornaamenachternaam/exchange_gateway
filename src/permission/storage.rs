// src/permission/storage.rs
use crate::permission::types::{CalendarPermission, DelegateInfo, PermissionAuditEntry};
use crate::storage::{AuditRow, DelegateRow, PermissionRow, Storage};
use anyhow::Result;

fn parse_sqlite_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&chrono::Utc);
    }
    match chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        Ok(dt) => chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc),
        Err(e) => {
            tracing::warn!(timestamp = %s, error = %e, "Failed to parse timestamp, using current time");
            chrono::Utc::now()
        }
    }
}

impl From<PermissionRow> for CalendarPermission {
    fn from(row: PermissionRow) -> Self {
        let rights = if row.rights >= 0 {
            row.rights as u32
        } else {
            tracing::warn!(
                folder_id = %row.folder_id,
                owner = %row.owner,
                user_email = %row.user_email,
                rights = row.rights,
                "Negative rights value in database, treating as 0"
            );
            0
        };
        Self {
            id: row.id,
            folder_id: row.folder_id,
            owner: row.owner,
            user_email: row.user_email,
            user_name: row.user_name,
            rights,
            is_default: row.is_default != 0,
            is_anonymous: row.is_anonymous != 0,
            created_at: parse_sqlite_timestamp(&row.created_at),
            updated_at: parse_sqlite_timestamp(&row.updated_at),
        }
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

impl From<AuditRow> for PermissionAuditEntry {
    fn from(row: AuditRow) -> Self {
        fn safe_i32_to_u32(v: i32) -> u32 {
            if v >= 0 { v as u32 } else { 0 }
        }
        Self {
            id: row.id,
            folder_id: row.folder_id,
            owner: row.owner,
            actor_email: row.actor_email,
            target_email: row.target_email,
            operation: row.operation,
            old_rights: row.old_rights.map(safe_i32_to_u32),
            new_rights: row.new_rights.map(safe_i32_to_u32),
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
        let row = self
            .storage
            .fetch_calendar_permission(owner, folder_id, user_email)
            .await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_permissions_for_folder(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let rows = self
            .storage
            .fetch_calendar_permissions(owner, folder_id)
            .await?;
        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn get_permissions_for_user(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let rows = self
            .storage
            .fetch_user_calendar_permissions(owner, user_email)
            .await?;
        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn upsert_permission(&self, permission: &CalendarPermission) -> Result<()> {
        self.storage
            .upsert_calendar_permission(&PermissionRow {
                id: permission.id.clone(),
                folder_id: permission.folder_id.clone(),
                owner: permission.owner.clone(),
                user_email: permission.user_email.clone(),
                user_name: permission.user_name.clone(),
                rights: permission.rights as i32,
                is_default: if permission.is_default { 1 } else { 0 },
                is_anonymous: if permission.is_anonymous { 1 } else { 0 },
                created_at: permission.created_at.to_rfc3339(),
                updated_at: permission.updated_at.to_rfc3339(),
            })
            .await
    }

    pub async fn delete_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<()> {
        self.storage
            .remove_calendar_permission(owner, folder_id, user_email)
            .await
    }

    pub async fn get_default_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let row = self
            .storage
            .fetch_default_calendar_permission(owner, folder_id)
            .await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_anonymous_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let row = self
            .storage
            .fetch_anonymous_calendar_permission(owner, folder_id)
            .await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateInfo>> {
        let row = self
            .storage
            .fetch_delegate(delegator, delegate_email)
            .await?;
        Ok(row.map(DelegateInfo::from))
    }

    pub async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let rows = self.storage.fetch_delegates(delegator).await?;
        Ok(rows.into_iter().map(DelegateInfo::from).collect())
    }

    pub async fn upsert_delegate(&self, delegate: &DelegateInfo) -> Result<()> {
        self.storage
            .upsert_delegate(&DelegateRow {
                id: delegate.id.clone(),
                delegator: delegate.delegator.clone(),
                delegate_email: delegate.delegate_email.clone(),
                delegate_name: delegate.delegate_name.clone(),
                calendar_permission: delegate.calendar_permission as i32,
                inbox_permission: delegate.inbox_permission as i32,
                tasks_permission: delegate.tasks_permission as i32,
                contacts_permission: delegate.contacts_permission as i32,
                notes_permission: delegate.notes_permission as i32,
                journal_permission: delegate.journal_permission as i32,
                receive_copies: if delegate.receive_copies { 1 } else { 0 },
                receive_infos: if delegate.receive_infos { 1 } else { 0 },
                view_private: if delegate.view_private { 1 } else { 0 },
                created_at: delegate.created_at.to_rfc3339(),
                updated_at: delegate.updated_at.to_rfc3339(),
            })
            .await
    }

    pub async fn delete_delegate(&self, delegator: &str, delegate_email: &str) -> Result<()> {
        self.storage
            .remove_delegate(delegator, delegate_email)
            .await
    }

    pub async fn add_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        self.storage
            .add_permission_audit(&AuditRow {
                id: entry.id.clone(),
                folder_id: entry.folder_id.clone(),
                owner: entry.owner.clone(),
                actor_email: entry.actor_email.clone(),
                target_email: entry.target_email.clone(),
                operation: entry.operation.clone(),
                old_rights: entry.old_rights.map(|v| v as i32),
                new_rights: entry.new_rights.map(|v| v as i32),
                created_at: entry.created_at.to_rfc3339(),
            })
            .await
    }

    pub async fn get_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<PermissionAuditEntry>> {
        let rows = self
            .storage
            .fetch_permission_audit_log(owner, folder_id, limit)
            .await?;
        Ok(rows.into_iter().map(PermissionAuditEntry::from).collect())
    }
}
