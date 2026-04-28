// src/storage.rs
use anyhow::Result;
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct Storage {
    pool: Pool<SqliteConnectionManager>,
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

#[derive(Deserialize, Serialize)]
pub struct JournalRow {
    pub seq: i64,
    pub server_id: String,
    pub op: String,
    pub resource_href: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct EwsItemRow {
    pub server_id: String,
    pub resource_href: String,
    pub uid: Option<String>,
    pub etag: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct CalendarExceptionRow {
    pub parent_server_id: String,
    pub exception_start: String,
    pub server_id: Option<String>,
    pub is_deleted: i32,
    pub created_at: String,
}

#[derive(Deserialize, Serialize)]
pub struct MeetingResponseRow {
    pub request_id: String,
    pub calendar_id: String,
    pub user_response: i32,
    pub created_at: String,
}

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
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

#[derive(Deserialize, Serialize)]
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
        if let Some(parent) = std::path::Path::new(db_path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let manager = SqliteConnectionManager::file(db_path).with_init(|conn| {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            conn.busy_timeout(std::time::Duration::from_secs(5))?;
            Ok(())
        });
        let pool = Pool::builder().max_size(16).build(manager)?;
        let storage = Self { pool };
        storage.initialize()?;
        Ok(storage)
    }

    fn initialize(&self) -> Result<()> {
        self.with_conn(|conn| {
            conn.execute_batch(include_str!("../schema.sql"))?;
            Ok(())
        })
    }

    fn with_conn<T, F>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut rusqlite::Connection) -> Result<T>,
    {
        let mut conn = self.pool.get()?;
        f(&mut conn)
    }

    pub async fn fetch_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<Option<PermissionRow>> {
        self.get_calendar_permission(owner, folder_id, user_email)
            .await
    }

    pub async fn fetch_calendar_permissions(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<PermissionRow>> {
        self.get_calendar_permissions(owner, folder_id).await
    }

    pub async fn fetch_user_calendar_permissions(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<PermissionRow>> {
        self.get_user_calendar_permissions(owner, user_email).await
    }

    pub async fn fetch_default_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<PermissionRow>> {
        self.get_default_calendar_permission(owner, folder_id).await
    }

    pub async fn fetch_anonymous_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<PermissionRow>> {
        self.get_anonymous_calendar_permission(owner, folder_id)
            .await
    }

    pub async fn upsert_calendar_permission(&self, row: &PermissionRow) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO calendar_permission (id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,CURRENT_TIMESTAMP) ON CONFLICT(owner,folder_id,user_email) DO UPDATE SET id=excluded.id,user_name=excluded.user_name,rights=excluded.rights,is_default=excluded.is_default,is_anonymous=excluded.is_anonymous,updated_at=CURRENT_TIMESTAMP",
                params![
                    row.id,
                    row.folder_id,
                    row.owner,
                    row.user_email,
                    row.user_name,
                    row.rights,
                    row.is_default,
                    row.is_anonymous
                ],
            )?;
            Ok(())
        })
    }

    pub async fn remove_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM calendar_permission WHERE owner=?1 AND folder_id=?2 AND user_email=?3",
                params![owner, folder_id, user_email],
            )?;
            Ok(())
        })
    }

    pub async fn fetch_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateRow>> {
        self.get_delegate(delegator, delegate_email).await
    }

    pub async fn fetch_delegates(&self, delegator: &str) -> Result<Vec<DelegateRow>> {
        self.get_delegates(delegator).await
    }

    pub async fn upsert_delegate(&self, row: &DelegateRow) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO calendar_delegate (id,delegator,delegate_email,delegate_name,calendar_permission,inbox_permission,tasks_permission,contacts_permission,notes_permission,journal_permission,receive_copies,receive_infos,view_private,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,CURRENT_TIMESTAMP) ON CONFLICT(delegator,delegate_email) DO UPDATE SET id=excluded.id,delegate_name=excluded.delegate_name,calendar_permission=excluded.calendar_permission,inbox_permission=excluded.inbox_permission,tasks_permission=excluded.tasks_permission,contacts_permission=excluded.contacts_permission,notes_permission=excluded.notes_permission,journal_permission=excluded.journal_permission,receive_copies=excluded.receive_copies,receive_infos=excluded.receive_infos,view_private=excluded.view_private,updated_at=CURRENT_TIMESTAMP",
                params![
                    row.id,
                    row.delegator,
                    row.delegate_email,
                    row.delegate_name,
                    row.calendar_permission,
                    row.inbox_permission,
                    row.tasks_permission,
                    row.contacts_permission,
                    row.notes_permission,
                    row.journal_permission,
                    row.receive_copies,
                    row.receive_infos,
                    row.view_private
                ],
            )?;
            Ok(())
        })
    }

    pub async fn remove_delegate(&self, delegator: &str, delegate_email: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM calendar_delegate WHERE delegator=?1 AND delegate_email=?2",
                params![delegator, delegate_email],
            )?;
            Ok(())
        })
    }

    pub async fn add_permission_audit(&self, row: &AuditRow) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO permission_audit (id,folder_id,owner,actor_email,target_email,operation,old_rights,new_rights,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,CURRENT_TIMESTAMP)",
                params![
                    row.id,
                    row.folder_id,
                    row.owner,
                    row.actor_email,
                    row.target_email,
                    row.operation,
                    row.old_rights,
                    row.new_rights
                ],
            )?;
            Ok(())
        })
    }

    pub async fn fetch_permission_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditRow>> {
        self.get_permission_audit_log(owner, folder_id, limit).await
    }

    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO sync_state (owner, collection_id, sync_key, token, updated_at) VALUES (?1,?2,?3,?4,CURRENT_TIMESTAMP) ON CONFLICT(owner,collection_id) DO UPDATE SET sync_key=excluded.sync_key, token=excluded.token, updated_at=CURRENT_TIMESTAMP",
                params![owner, collection_id, sync_key, token],
            )?;
            Ok(())
        })
    }

    pub async fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT sync_key, token FROM sync_state WHERE owner=?1 AND collection_id=?2",
                params![owner, collection_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(Into::into)
        })
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
        self.with_conn(|c| {
            let tx = c.transaction()?;
            tx.execute("INSERT INTO item_map (owner, caldav_href, resource_href, server_id, uid, etag, updated_at) VALUES (?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP) ON CONFLICT(owner,server_id) DO UPDATE SET caldav_href=excluded.caldav_href,resource_href=excluded.resource_href,uid=excluded.uid,etag=excluded.etag,updated_at=CURRENT_TIMESTAMP", params![owner, caldav_href, resource_href, server_id, uid, etag])?;
            tx.execute("INSERT INTO change_journal (owner, server_id, op, resource_href, created_at) VALUES (?1,?2,'upsert',?3,CURRENT_TIMESTAMP)", params![owner, server_id, resource_href])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub async fn get_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        self.with_conn(|c| c.query_row("SELECT server_id, status FROM client_sync_command WHERE owner=?1 AND collection_id=?2 AND client_id=?3", params![owner, collection_id, client_id], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(Into::into))
    }

    pub async fn put_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
        server_id: Option<&str>,
        status: &str,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute("INSERT INTO client_sync_command (owner,collection_id,client_id,server_id,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP) ON CONFLICT(owner,collection_id,client_id) DO UPDATE SET server_id=excluded.server_id,status=excluded.status,updated_at=CURRENT_TIMESTAMP", params![owner, collection_id, client_id, server_id, status])?;
            Ok(())
        })
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        self.with_conn(|c| {
            let tx = c.transaction()?;
            tx.execute(
                "DELETE FROM item_map WHERE owner=?1 AND server_id=?2",
                params![owner, server_id],
            )?;
            tx.execute("INSERT INTO change_journal (owner, server_id, op, created_at) VALUES (?1,?2,'delete',CURRENT_TIMESTAMP)", params![owner, server_id])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO deleted_item_tombstone (owner, server_id, deleted_at) VALUES (?1,?2,CURRENT_TIMESTAMP) ON CONFLICT(owner,server_id) DO UPDATE SET deleted_at=CURRENT_TIMESTAMP", params![owner, server_id])?; Ok(()) })
    }

    pub async fn list_changes_since(
        &self,
        owner: &str,
        since_unix_ts: i64,
    ) -> Result<Vec<(String, String)>> {
        self.with_conn(|c| {
            let mut stmt = c.prepare("SELECT server_id, COALESCE(resource_href,'') FROM change_journal WHERE owner=?1 AND strftime('%s', created_at) >= ?2 AND op='upsert' ORDER BY id")?;
            let rows = stmt.query_map(params![owner, since_unix_ts], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub async fn list_deleted_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<String>> {
        self.with_conn(|c| {
            let mut s = c.prepare(
                "SELECT server_id FROM change_journal WHERE owner=?1 AND strftime('%s', created_at) >= ?2 AND op='delete' ORDER BY id",
            )?;
            let rows = s.query_map(params![owner, since_unix_ts], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        self.with_conn(|c| {
            c.query_row("SELECT COALESCE(MAX(id),0) FROM change_journal", [], |r| {
                r.get(0)
            })
            .map_err(Into::into)
        })
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, String, Option<String>)>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id, server_id, resource_href FROM change_journal WHERE owner=?1 AND id>?2 AND op='upsert' ORDER BY id")?; let rows=s.query_map(params![owner,since_seq],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn list_journal_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
        until_seq: i64,
        limit: usize,
    ) -> Result<Vec<JournalRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id, server_id, op, resource_href FROM change_journal WHERE owner=?1 AND id>?2 AND id<=?3 ORDER BY id LIMIT ?4")?; let rows=s.query_map(params![owner,since_seq,until_seq,limit as i64],|r| Ok(JournalRow{seq:r.get(0)?,server_id:r.get(1)?,op:r.get(2)?,resource_href:r.get(3)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn list_deleted_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, String)>> {
        self.with_conn(|c| {
            let mut s = c.prepare(
                "SELECT id, server_id FROM change_journal WHERE owner=?1 AND id>?2 AND op='delete' ORDER BY id",
            )?;
            let rows = s.query_map(params![owner, since_seq], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
        })
    }

    pub async fn set_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
        policy_key: &str,
        policy_status: &str,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO provision_state (owner,device_id,policy_key,policy_status,updated_at) VALUES (?1,?2,?3,?4,CURRENT_TIMESTAMP) ON CONFLICT(owner,device_id) DO UPDATE SET policy_key=excluded.policy_key,policy_status=excluded.policy_status,updated_at=CURRENT_TIMESTAMP", params![owner,device_id,policy_key,policy_status])?; Ok(()) })
    }

    pub async fn get_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<(String, String)>> {
        self.with_conn(|c| c.query_row("SELECT policy_key, policy_status FROM provision_state WHERE owner=?1 AND device_id=?2", params![owner,device_id], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(Into::into))
    }

    pub async fn upsert_device_info(&self, paramsx: &DeviceInfoParams<'_>) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO device_info (user_email,device_id,friendly_name,model,os,phone_number,imei,user_agent,last_seen) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,CURRENT_TIMESTAMP) ON CONFLICT(user_email,device_id) DO UPDATE SET friendly_name=excluded.friendly_name,model=excluded.model,os=excluded.os,phone_number=excluded.phone_number,imei=excluded.imei,user_agent=excluded.user_agent,last_seen=CURRENT_TIMESTAMP", params![paramsx.owner,paramsx.device_id,paramsx.friendly_name,paramsx.model,paramsx.os,paramsx.phone_number,paramsx.imei,paramsx.user_agent])?; Ok(()) })
    }

    pub async fn list_ews_items(
        &self,
        owner: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EwsItemRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT server_id, resource_href, uid, etag, updated_at FROM item_map WHERE owner=?1 ORDER BY updated_at DESC LIMIT ?2 OFFSET ?3")?; let rows=s.query_map(params![owner,limit as i64,offset as i64],|r| Ok(EwsItemRow{server_id:r.get(0)?,resource_href:r.get(1)?,uid:r.get(2)?,etag:r.get(3)?,updated_at:r.get(4)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn get_ews_sync_state(&self, owner: &str, folder_id: &str) -> Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT sync_state FROM ews_sync_state WHERE user_email=?1 AND folder_id=?2",
                params![owner, folder_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        self.with_conn(|c| c.query_row("SELECT server_id,resource_href,uid,etag,updated_at FROM item_map WHERE owner=?1 AND server_id=?2", params![owner,server_id], |r| Ok(EwsItemRow{server_id:r.get(0)?,resource_href:r.get(1)?,uid:r.get(2)?,etag:r.get(3)?,updated_at:r.get(4)?})).optional().map_err(Into::into))
    }

    pub async fn get_ews_item_owner(&self, server_id: &str) -> Result<Option<String>> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT owner FROM item_map WHERE server_id=?1 LIMIT 1",
                params![server_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })
    }

    pub async fn set_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
        sync_state: &str,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO ews_sync_state (user_email,folder_id,sync_state,created_at) VALUES (?1,?2,?3,CURRENT_TIMESTAMP) ON CONFLICT(user_email,folder_id) DO UPDATE SET sync_state=excluded.sync_state", params![owner,folder_id,sync_state])?; Ok(()) })
    }

    pub async fn upsert_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
        server_id: Option<&str>,
        is_deleted: bool,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO calendar_exceptions (owner,parent_server_id,exception_start,server_id,is_deleted,created_at) VALUES (?1,?2,?3,?4,?5,CURRENT_TIMESTAMP) ON CONFLICT(owner,parent_server_id,exception_start) DO UPDATE SET server_id=excluded.server_id,is_deleted=excluded.is_deleted", params![owner,parent_server_id,exception_start,server_id,if is_deleted {1}else{0}])?; Ok(()) })
    }

    pub async fn get_calendar_exceptions(
        &self,
        owner: &str,
        parent_server_id: &str,
    ) -> Result<Vec<CalendarExceptionRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner=?1 AND parent_server_id=?2 ORDER BY exception_start")?; let rows=s.query_map(params![owner,parent_server_id],|r| Ok(CalendarExceptionRow{parent_server_id:r.get(0)?,exception_start:r.get(1)?,server_id:r.get(2)?,is_deleted:r.get(3)?,created_at:r.get(4)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn get_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<Option<CalendarExceptionRow>> {
        self.with_conn(|c| c.query_row("SELECT parent_server_id, exception_start, server_id, is_deleted, created_at FROM calendar_exceptions WHERE owner=?1 AND parent_server_id=?2 AND exception_start=?3", params![owner,parent_server_id,exception_start], |r| Ok(CalendarExceptionRow{parent_server_id:r.get(0)?,exception_start:r.get(1)?,server_id:r.get(2)?,is_deleted:r.get(3)?,created_at:r.get(4)?})).optional().map_err(Into::into))
    }

    pub async fn delete_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("DELETE FROM calendar_exceptions WHERE owner=?1 AND parent_server_id=?2 AND exception_start=?3", params![owner,parent_server_id,exception_start])?; Ok(()) })
    }

    pub async fn record_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
        calendar_id: &str,
        user_response: i32,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO meeting_response (owner,request_id,calendar_id,user_response,created_at) VALUES (?1,?2,?3,?4,CURRENT_TIMESTAMP) ON CONFLICT(owner,request_id) DO UPDATE SET calendar_id=excluded.calendar_id,user_response=excluded.user_response", params![owner,request_id,calendar_id,user_response])?; Ok(()) })
    }

    pub async fn get_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<MeetingResponseRow>> {
        self.with_conn(|c| c.query_row("SELECT request_id, calendar_id, user_response, created_at FROM meeting_response WHERE owner=?1 AND request_id=?2", params![owner,request_id], |r| Ok(MeetingResponseRow{request_id:r.get(0)?,calendar_id:r.get(1)?,user_response:r.get(2)?,created_at:r.get(3)?})).optional().map_err(Into::into))
    }

    pub async fn upsert_meeting_state(&self, p: &MeetingStateParams<'_>) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO meeting_state (uid,owner,sequence,state,state_flags,is_organizer,organizer_email,organizer_name,subject,location,start_time,end_time,timezone,created_at,updated_at,last_sequence_time) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP) ON CONFLICT(owner,uid) DO UPDATE SET sequence=excluded.sequence,state=excluded.state,state_flags=excluded.state_flags,is_organizer=excluded.is_organizer,organizer_email=excluded.organizer_email,organizer_name=excluded.organizer_name,subject=excluded.subject,location=excluded.location,start_time=excluded.start_time,end_time=excluded.end_time,timezone=excluded.timezone,updated_at=CURRENT_TIMESTAMP,last_sequence_time=CURRENT_TIMESTAMP", params![p.uid,p.owner,p.sequence,p.state,p.state_flags,if p.is_organizer{1}else{0},p.organizer_email,p.organizer_name,p.subject,p.location,p.start_time,p.end_time,p.timezone])?; Ok(()) })
    }

    pub async fn get_meeting_state(
        &self,
        owner: &str,
        uid: &str,
    ) -> Result<Option<MeetingStateRow>> {
        self.with_conn(|c| c.query_row("SELECT uid,owner,sequence,state,state_flags,is_organizer,organizer_email,organizer_name,subject,location,start_time,end_time,timezone,created_at,updated_at,last_sequence_time FROM meeting_state WHERE owner=?1 AND uid=?2", params![owner,uid], meeting_state_from_row).optional().map_err(Into::into))
    }

    pub async fn delete_meeting_state(&self, owner: &str, uid: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM meeting_state WHERE owner=?1 AND uid=?2",
                params![owner, uid],
            )?;
            Ok(())
        })
    }

    pub async fn upsert_meeting_attendee(&self, p: &MeetingAttendeeParams<'_>) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO meeting_attendee (meeting_uid,owner,email,name,status,role,response_time,proposed_start,proposed_end,sequence,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,CURRENT_TIMESTAMP,CURRENT_TIMESTAMP) ON CONFLICT(owner,meeting_uid,email) DO UPDATE SET name=excluded.name,status=excluded.status,role=excluded.role,response_time=excluded.response_time,proposed_start=excluded.proposed_start,proposed_end=excluded.proposed_end,sequence=excluded.sequence,updated_at=CURRENT_TIMESTAMP", params![p.meeting_uid,p.owner,p.email,p.name,p.status,p.role,p.response_time,p.proposed_start,p.proposed_end,p.sequence])?; Ok(()) })
    }

    pub async fn get_meeting_attendees(
        &self,
        owner: &str,
        meeting_uid: &str,
    ) -> Result<Vec<MeetingAttendeeRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT meeting_uid,owner,email,name,status,role,response_time,proposed_start,proposed_end,sequence,created_at,updated_at FROM meeting_attendee WHERE owner=?1 AND meeting_uid=?2")?; let rows=s.query_map(params![owner,meeting_uid],|r| Ok(MeetingAttendeeRow{meeting_uid:r.get(0)?,owner:r.get(1)?,email:r.get(2)?,name:r.get(3)?,status:r.get(4)?,role:r.get(5)?,response_time:r.get(6)?,proposed_start:r.get(7)?,proposed_end:r.get(8)?,sequence:r.get(9)?,created_at:r.get(10)?,updated_at:r.get(11)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn delete_meeting_attendee(
        &self,
        owner: &str,
        meeting_uid: &str,
        email: &str,
    ) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM meeting_attendee WHERE owner=?1 AND meeting_uid=?2 AND email=?3",
                params![owner, meeting_uid, email],
            )?;
            Ok(())
        })
    }
    pub async fn delete_meeting_attendees(&self, owner: &str, meeting_uid: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM meeting_attendee WHERE owner=?1 AND meeting_uid=?2",
                params![owner, meeting_uid],
            )?;
            Ok(())
        })
    }

    pub async fn enqueue_scheduling(
        &self,
        owner: &str,
        meeting_uid: &str,
        operation: &str,
        sequence: u32,
        ical_data: &str,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO meeting_scheduling_queue (meeting_uid,owner,operation,sequence,ical_data,status,created_at) VALUES (?1,?2,?3,?4,?5,'pending',CURRENT_TIMESTAMP)", params![meeting_uid,owner,operation,sequence,ical_data])?; Ok(()) })
    }

    pub async fn get_pending_scheduling(
        &self,
        owner: &str,
        limit: usize,
    ) -> Result<Vec<SchedulingQueueRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,meeting_uid,owner,operation,sequence,ical_data,status,attempts,last_attempt,error_message,created_at,processed_at FROM meeting_scheduling_queue WHERE owner=?1 AND status='pending' ORDER BY id LIMIT ?2")?; let rows=s.query_map(params![owner,limit as i64],|r| Ok(SchedulingQueueRow{id:r.get(0)?,meeting_uid:r.get(1)?,owner:r.get(2)?,operation:r.get(3)?,sequence:r.get(4)?,ical_data:r.get(5)?,status:r.get(6)?,attempts:r.get(7)?,last_attempt:r.get(8)?,error_message:r.get(9)?,created_at:r.get(10)?,processed_at:r.get(11)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn mark_scheduling_processed(
        &self,
        id: i64,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("UPDATE meeting_scheduling_queue SET status=?2,error_message=?3,processed_at=CURRENT_TIMESTAMP,last_attempt=CURRENT_TIMESTAMP,attempts=attempts+1 WHERE id=?1", params![id,status,error_message])?; Ok(()) })
    }

    pub async fn get_meetings_by_time_range(
        &self,
        owner: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<MeetingStateRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT uid,owner,sequence,state,state_flags,is_organizer,organizer_email,organizer_name,subject,location,start_time,end_time,timezone,created_at,updated_at,last_sequence_time FROM meeting_state WHERE owner=?1 AND start_time < ?3 AND end_time > ?2 ORDER BY start_time")?; let rows=s.query_map(params![owner,start,end], meeting_state_from_row)?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn upsert_calendar_attachment(
        &self,
        a: &crate::attachment::AttachmentRecord,
    ) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO calendar_attachment (id,parent_item_server_id,owner,name,content_type,content_size,content_base64,is_inline,content_id,content_location,attachment_type,last_modified_time,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,CURRENT_TIMESTAMP) ON CONFLICT(id) DO UPDATE SET parent_item_server_id=excluded.parent_item_server_id,owner=excluded.owner,name=excluded.name,content_type=excluded.content_type,content_size=excluded.content_size,content_base64=excluded.content_base64,is_inline=excluded.is_inline,content_id=excluded.content_id,content_location=excluded.content_location,attachment_type=excluded.attachment_type,last_modified_time=excluded.last_modified_time,updated_at=CURRENT_TIMESTAMP", params![a.id,a.parent_item_server_id,a.owner,a.name,a.content_type,a.content_size,a.content_base64,if a.is_inline{1}else{0},a.content_id,a.content_location,a.attachment_type.as_str(),a.last_modified_time])?; Ok(()) })
    }

    pub async fn get_calendar_attachment(
        &self,
        owner: &str,
        attachment_id: &str,
    ) -> Result<Option<crate::attachment::AttachmentRecord>> {
        self.with_conn(|c| c.query_row("SELECT id,parent_item_server_id,owner,name,content_type,content_size,content_base64,is_inline,content_id,content_location,attachment_type,last_modified_time FROM calendar_attachment WHERE owner=?1 AND id=?2", params![owner,attachment_id], attachment_row).optional().map_err(Into::into))
    }

    pub async fn get_calendar_attachments_for_item(
        &self,
        owner: &str,
        parent_item_server_id: &str,
    ) -> Result<Vec<crate::attachment::AttachmentRecord>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,parent_item_server_id,owner,name,content_type,content_size,content_base64,is_inline,content_id,content_location,attachment_type,last_modified_time FROM calendar_attachment WHERE owner=?1 AND parent_item_server_id=?2")?; let rows=s.query_map(params![owner,parent_item_server_id], attachment_row)?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }

    pub async fn delete_calendar_attachment(&self, owner: &str, attachment_id: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute(
                "DELETE FROM calendar_attachment WHERE owner=?1 AND id=?2",
                params![owner, attachment_id],
            )?;
            Ok(())
        })
    }

    pub async fn upsert_room_list(&self, room_list: &crate::room::RoomListRecord) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO room_list (id,email,name,updated_at) VALUES (?1,?2,?3,CURRENT_TIMESTAMP) ON CONFLICT(email) DO UPDATE SET id=excluded.id,name=excluded.name,updated_at=CURRENT_TIMESTAMP", params![room_list.id,room_list.email,room_list.name])?; Ok(()) })
    }
    pub async fn get_room_lists(&self, _owner: &str) -> Result<Vec<crate::room::RoomListRecord>> {
        self.with_conn(|c| {
            let mut s = c.prepare("SELECT id,email,name FROM room_list ORDER BY name")?;
            let rows = s.query_map([], |r| {
                Ok(crate::room::RoomListRecord {
                    id: r.get(0)?,
                    email: r.get(1)?,
                    name: r.get(2)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
    }
    pub async fn delete_room_list(&self, _owner: &str, email: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute("DELETE FROM room_list WHERE email=?1", params![email])?;
            Ok(())
        })
    }
    pub async fn get_all_rooms(&self, _owner: &str) -> Result<Vec<crate::room::RoomRecord>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,room_list_email,email,name,capacity,is_available FROM room ORDER BY name")?; let rows=s.query_map([], room_row)?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
    pub async fn delete_room(&self, _owner: &str, email: &str) -> Result<()> {
        self.with_conn(|c| {
            c.execute("DELETE FROM room WHERE email=?1", params![email])?;
            Ok(())
        })
    }
    pub async fn upsert_room(&self, room: &crate::room::RoomRecord) -> Result<()> {
        self.with_conn(|c| { c.execute("INSERT INTO room (id,room_list_email,email,name,capacity,is_available,updated_at) VALUES (?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP) ON CONFLICT(email) DO UPDATE SET id=excluded.id,room_list_email=excluded.room_list_email,name=excluded.name,capacity=excluded.capacity,is_available=excluded.is_available,updated_at=CURRENT_TIMESTAMP", params![room.id,room.room_list_email,room.email,room.name,room.capacity,if room.is_available{1}else{0}])?; Ok(()) })
    }
    pub async fn get_rooms_for_list(
        &self,
        _owner: &str,
        room_list_email: &str,
    ) -> Result<Vec<crate::room::RoomRecord>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,room_list_email,email,name,capacity,is_available FROM room WHERE room_list_email=?1 ORDER BY name")?; let rows=s.query_map(params![room_list_email], room_row)?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
}

fn meeting_state_from_row(r: &Row<'_>) -> rusqlite::Result<MeetingStateRow> {
    Ok(MeetingStateRow {
        uid: r.get(0)?,
        owner: r.get(1)?,
        sequence: r.get(2)?,
        state: r.get(3)?,
        state_flags: r.get(4)?,
        is_organizer: r.get(5)?,
        organizer_email: r.get(6)?,
        organizer_name: r.get(7)?,
        subject: r.get(8)?,
        location: r.get(9)?,
        start_time: r.get(10)?,
        end_time: r.get(11)?,
        timezone: r.get(12)?,
        created_at: r.get(13)?,
        updated_at: r.get(14)?,
        last_sequence_time: r.get(15)?,
    })
}

fn attachment_row(r: &Row<'_>) -> rusqlite::Result<crate::attachment::AttachmentRecord> {
    Ok(crate::attachment::AttachmentRecord {
        id: r.get(0)?,
        parent_item_server_id: r.get(1)?,
        owner: r.get(2)?,
        name: r.get(3)?,
        content_type: r.get(4)?,
        content_size: r.get(5)?,
        content_base64: r.get(6)?,
        is_inline: r.get::<_, i32>(7)? != 0,
        content_id: r.get(8)?,
        content_location: r.get(9)?,
        attachment_type: crate::attachment::AttachmentType::from(r.get::<_, String>(10)?.as_str()),
        last_modified_time: r.get(11)?,
    })
}

fn room_row(r: &Row<'_>) -> rusqlite::Result<crate::room::RoomRecord> {
    Ok(crate::room::RoomRecord {
        id: r.get(0)?,
        room_list_email: r.get(1)?,
        email: r.get(2)?,
        name: r.get(3)?,
        capacity: r.get(4)?,
        is_available: r.get::<_, i32>(5)? != 0,
    })
}

#[derive(Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
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

impl Storage {
    async fn get_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
        user_email: &str,
    ) -> Result<Option<PermissionRow>> {
        self.with_conn(|c| c.query_row("SELECT id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,created_at,updated_at FROM calendar_permission WHERE owner=?1 AND folder_id=?2 AND user_email=?3",
            params![owner,folder_id,user_email], |r| Ok(PermissionRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,user_email:r.get(3)?,user_name:r.get(4)?,rights:r.get(5)?,is_default:r.get(6)?,is_anonymous:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?})).optional().map_err(Into::into))
    }
    async fn get_calendar_permissions(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Vec<PermissionRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,created_at,updated_at FROM calendar_permission WHERE owner=?1 AND folder_id=?2 ORDER BY user_email")?; let rows=s.query_map(params![owner,folder_id], |r| Ok(PermissionRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,user_email:r.get(3)?,user_name:r.get(4)?,rights:r.get(5)?,is_default:r.get(6)?,is_anonymous:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
    async fn get_user_calendar_permissions(
        &self,
        owner: &str,
        user_email: &str,
    ) -> Result<Vec<PermissionRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,created_at,updated_at FROM calendar_permission WHERE owner=?1 AND user_email=?2 ORDER BY folder_id")?; let rows=s.query_map(params![owner,user_email], |r| Ok(PermissionRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,user_email:r.get(3)?,user_name:r.get(4)?,rights:r.get(5)?,is_default:r.get(6)?,is_anonymous:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
    async fn get_default_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<PermissionRow>> {
        self.with_conn(|c| c.query_row("SELECT id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,created_at,updated_at FROM calendar_permission WHERE owner=?1 AND folder_id=?2 AND is_default=1 LIMIT 1", params![owner,folder_id], |r| Ok(PermissionRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,user_email:r.get(3)?,user_name:r.get(4)?,rights:r.get(5)?,is_default:r.get(6)?,is_anonymous:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?})).optional().map_err(Into::into))
    }
    async fn get_anonymous_calendar_permission(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<PermissionRow>> {
        self.with_conn(|c| c.query_row("SELECT id,folder_id,owner,user_email,user_name,rights,is_default,is_anonymous,created_at,updated_at FROM calendar_permission WHERE owner=?1 AND folder_id=?2 AND is_anonymous=1 LIMIT 1", params![owner,folder_id], |r| Ok(PermissionRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,user_email:r.get(3)?,user_name:r.get(4)?,rights:r.get(5)?,is_default:r.get(6)?,is_anonymous:r.get(7)?,created_at:r.get(8)?,updated_at:r.get(9)?})).optional().map_err(Into::into))
    }
    async fn get_delegate(
        &self,
        delegator: &str,
        delegate_email: &str,
    ) -> Result<Option<DelegateRow>> {
        self.with_conn(|c| c.query_row("SELECT id,delegator,delegate_email,delegate_name,calendar_permission,inbox_permission,tasks_permission,contacts_permission,notes_permission,journal_permission,receive_copies,receive_infos,view_private,created_at,updated_at FROM calendar_delegate WHERE delegator=?1 AND delegate_email=?2", params![delegator,delegate_email], |r| Ok(DelegateRow{id:r.get(0)?,delegator:r.get(1)?,delegate_email:r.get(2)?,delegate_name:r.get(3)?,calendar_permission:r.get(4)?,inbox_permission:r.get(5)?,tasks_permission:r.get(6)?,contacts_permission:r.get(7)?,notes_permission:r.get(8)?,journal_permission:r.get(9)?,receive_copies:r.get(10)?,receive_infos:r.get(11)?,view_private:r.get(12)?,created_at:r.get(13)?,updated_at:r.get(14)?})).optional().map_err(Into::into))
    }
    async fn get_delegates(&self, delegator: &str) -> Result<Vec<DelegateRow>> {
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,delegator,delegate_email,delegate_name,calendar_permission,inbox_permission,tasks_permission,contacts_permission,notes_permission,journal_permission,receive_copies,receive_infos,view_private,created_at,updated_at FROM calendar_delegate WHERE delegator=?1 ORDER BY delegate_email")?; let rows=s.query_map(params![delegator], |r| Ok(DelegateRow{id:r.get(0)?,delegator:r.get(1)?,delegate_email:r.get(2)?,delegate_name:r.get(3)?,calendar_permission:r.get(4)?,inbox_permission:r.get(5)?,tasks_permission:r.get(6)?,contacts_permission:r.get(7)?,notes_permission:r.get(8)?,journal_permission:r.get(9)?,receive_copies:r.get(10)?,receive_infos:r.get(11)?,view_private:r.get(12)?,created_at:r.get(13)?,updated_at:r.get(14)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
    async fn get_permission_audit_log(
        &self,
        owner: &str,
        folder_id: &str,
        limit: usize,
    ) -> Result<Vec<AuditRow>> {
        let limit_num = limit as i64;
        self.with_conn(|c| { let mut s=c.prepare("SELECT id,folder_id,owner,actor_email,target_email,operation,old_rights,new_rights,created_at FROM permission_audit WHERE owner=?1 AND folder_id=?2 ORDER BY created_at DESC LIMIT ?3")?; let rows=s.query_map(params![owner,folder_id,limit_num], |r| Ok(AuditRow{id:r.get(0)?,folder_id:r.get(1)?,owner:r.get(2)?,actor_email:r.get(3)?,target_email:r.get(4)?,operation:r.get(5)?,old_rights:r.get(6)?,new_rights:r.get(7)?,created_at:r.get(8)?}))?; rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into) })
    }
}
