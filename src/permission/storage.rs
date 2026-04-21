// src/permission/storage.rs
use crate::permission::types::{CalendarPermission, DelegateInfo, PermissionAuditEntry};
use crate::storage::Storage;
use anyhow::Result;
use serde::{Deserialize, Serialize};

fn parse_sqlite_timestamp(s: &str) -> chrono::DateTime<chrono::Utc> {
    // Try RFC3339 first (in case format changes)
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&chrono::Utc);
    }
    // Try SQLite's default format: "YYYY-MM-DD HH:MM:SS"
    match chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
        Ok(dt) => chrono::DateTime::from_naive_utc_and_offset(dt, chrono::Utc),
        Err(e) => {
            tracing::warn!(timestamp = %s, error = %e, "Failed to parse timestamp, using current time");
            chrono::Utc::now()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRow {
    pub id: String,
    pub folder_id: String,
    pub owner: String,
    pub user_email: String,
    pub user_name: Option<String>,
    pub rights: i32,
    pub is_default: i32,
    pub is_anonymous: i32,
    pub created_at: String,
    pub updated_at: String,
}

impl From<PermissionRow> for CalendarPermission {
    fn from(row: PermissionRow) -> Self {
        // Cast i32 to u32 safely - negative values are invalid but won't panic
        // They represent corrupted data from direct DB manipulation
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DelegateRow {
    pub id: String,
    pub delegator: String,
    pub delegate_email: String,
    pub delegate_name: Option<String>,
    pub calendar_permission: i32,
    pub inbox_permission: i32,
    pub tasks_permission: i32,
    pub contacts_permission: i32,
    pub notes_permission: i32,
    pub journal_permission: i32,
    pub receive_copies: i32,
    pub receive_infos: i32,
    pub view_private: i32,
    pub created_at: String,
    pub updated_at: String,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditRow {
    pub id: String,
    pub folder_id: String,
    pub owner: String,
    pub actor_email: String,
    pub target_email: String,
    pub operation: String,
    pub old_rights: Option<i32>,
    pub new_rights: Option<i32>,
    pub created_at: String,
}

impl From<AuditRow> for PermissionAuditEntry {
    fn from(row: AuditRow) -> Self {
        // Safely cast i32 to u32, treating negative values as 0
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
        let path = format!(
            "get_calendar_permission?owner={}&folder_id={}&user_email={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id),
            urlencoding::encode(user_email)
        );
        let row: Option<PermissionRow> = self.storage.get_json(&path).await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_permissions_for_folder(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let path = format!(
            "get_calendar_permissions?owner={}&folder_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id)
        );
        let rows: Vec<PermissionRow> = self.storage.get_json(&path).await?;
        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn get_permissions_for_user(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<CalendarPermission>> {
        let path = format!(
            "get_user_calendar_permissions?owner={}&user_email={}",
            urlencoding::encode(owner),
            urlencoding::encode(user_email)
        );
        let rows: Vec<PermissionRow> = self.storage.get_json(&path).await?;
        Ok(rows.into_iter().map(CalendarPermission::from).collect())
    }

    pub async fn upsert_permission(&self, permission: &CalendarPermission) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            id: &'a str,
            folder_id: &'a str,
            owner: &'a str,
            user_email: &'a str,
            user_name: Option<&'a str>,
            rights: i32,
            is_default: i32,
            is_anonymous: i32,
        }
        self.storage
            .post_json(
                "upsert_calendar_permission",
                &Req {
                    id: &permission.id,
                    folder_id: &permission.folder_id,
                    owner: &permission.owner,
                    user_email: &permission.user_email,
                    user_name: permission.user_name.as_deref(),
                    rights: permission.rights as i32,
                    is_default: if permission.is_default { 1 } else { 0 },
                    is_anonymous: if permission.is_anonymous { 1 } else { 0 },
                },
            )
            .await
    }

    pub async fn delete_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            folder_id: &'a str,
            user_email: &'a str,
        }
        self.storage
            .post_json(
                "delete_calendar_permission",
                &Req {
                    owner,
                    folder_id,
                    user_email,
                },
            )
            .await
    }

    pub async fn get_default_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let path = format!(
            "get_default_calendar_permission?owner={}&folder_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id)
        );
        let row: Option<PermissionRow> = self.storage.get_json(&path).await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_anonymous_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<CalendarPermission>> {
        let path = format!(
            "get_anonymous_calendar_permission?owner={}&folder_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id)
        );
        let row: Option<PermissionRow> = self.storage.get_json(&path).await?;
        Ok(row.map(CalendarPermission::from))
    }

    pub async fn get_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateInfo>> {
        let path = format!(
            "get_delegate?delegator={}&delegate_email={}",
            urlencoding::encode(delegator),
            urlencoding::encode(delegate_email)
        );
        let row: Option<DelegateRow> = self.storage.get_json(&path).await?;
        Ok(row.map(DelegateInfo::from))
    }

    pub async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateInfo>> {
        let path = format!("get_delegates?delegator={}", urlencoding::encode(delegator));
        let rows: Vec<DelegateRow> = self.storage.get_json(&path).await?;
        Ok(rows.into_iter().map(DelegateInfo::from).collect())
    }

    pub async fn upsert_delegate(&self, delegate: &DelegateInfo) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            id: &'a str,
            delegator: &'a str,
            delegate_email: &'a str,
            delegate_name: Option<&'a str>,
            calendar_permission: i32,
            inbox_permission: i32,
            tasks_permission: i32,
            contacts_permission: i32,
            notes_permission: i32,
            journal_permission: i32,
            receive_copies: i32,
            receive_infos: i32,
            view_private: i32,
        }
        self.storage
            .post_json(
                "upsert_delegate",
                &Req {
                    id: &delegate.id,
                    delegator: &delegate.delegator,
                    delegate_email: &delegate.delegate_email,
                    delegate_name: delegate.delegate_name.as_deref(),
                    calendar_permission: delegate.calendar_permission as i32,
                    inbox_permission: delegate.inbox_permission as i32,
                    tasks_permission: delegate.tasks_permission as i32,
                    contacts_permission: delegate.contacts_permission as i32,
                    notes_permission: delegate.notes_permission as i32,
                    journal_permission: delegate.journal_permission as i32,
                    receive_copies: if delegate.receive_copies { 1 } else { 0 },
                    receive_infos: if delegate.receive_infos { 1 } else { 0 },
                    view_private: if delegate.view_private { 1 } else { 0 },
                },
            )
            .await
    }

    pub async fn delete_delegate(&self, delegator: &str, delegate_email: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            delegator: &'a str,
            delegate_email: &'a str,
        }
        self.storage
            .post_json(
                "delete_delegate",
                &Req {
                    delegator,
                    delegate_email,
                },
            )
            .await
    }

    pub async fn add_audit_entry(&self, entry: &PermissionAuditEntry) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            id: &'a str,
            folder_id: &'a str,
            owner: &'a str,
            actor_email: &'a str,
            target_email: &'a str,
            operation: &'a str,
            old_rights: Option<i32>,
            new_rights: Option<i32>,
        }
        self.storage
            .post_json(
                "add_permission_audit",
                &Req {
                    id: &entry.id,
                    folder_id: &entry.folder_id,
                    owner: &entry.owner,
                    actor_email: &entry.actor_email,
                    target_email: &entry.target_email,
                    operation: &entry.operation,
                    old_rights: entry.old_rights.map(|v| v as i32),
                    new_rights: entry.new_rights.map(|v| v as i32),
                },
            )
            .await
    }

    pub async fn get_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<PermissionAuditEntry>> {
        let path = format!(
            "get_permission_audit_log?owner={}&folder_id={}&limit={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id),
            limit
        );
        let rows: Vec<AuditRow> = self.storage.get_json(&path).await?;
        Ok(rows.into_iter().map(PermissionAuditEntry::from).collect())
    }
}
