// src/storage.rs
use anyhow::{Result, anyhow};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use reqwest_tracing::TracingMiddleware;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone)]
pub struct Storage {
    client: ClientWithMiddleware,
    base_url: String,
    secret: String,
}

#[derive(Serialize)]
struct SetSyncKeyReq<'a> {
    owner: &'a str,
    collection_id: &'a str,
    sync_key: &'a str,
    token: Option<&'a str>,
}

#[derive(Serialize)]
struct UpsertItemReq<'a> {
    owner: &'a str,
    caldav_href: &'a str,
    resource_href: &'a str,
    server_id: &'a str,
    uid: &'a str,
    etag: &'a str,
}

#[derive(Serialize)]
struct SetProvisionReq<'a> {
    owner: &'a str,
    device_id: &'a str,
    policy_key: &'a str,
    policy_status: &'a str,
}

#[derive(Deserialize)]
struct ProvisionRow {
    policy_key: String,
    policy_status: String,
}

#[derive(Deserialize)]
struct ChangeRow {
    seq: Option<i64>,
    server_id: String,
    resource_href: Option<String>,
}

#[derive(Deserialize)]
struct SyncKeyRow {
    sync_key: String,
    token: Option<String>,
}

#[derive(Deserialize)]
struct ClientSyncCommandRow {
    server_id: Option<String>,
    status: String,
}

#[derive(Deserialize)]
struct TombstoneRow {
    seq: Option<i64>,
    server_id: String,
}

#[derive(Deserialize)]
struct LatestSeqRow {
    seq: i64,
}

#[derive(Deserialize)]
pub struct JournalRow {
    pub seq: i64,
    pub server_id: String,
    pub op: String,
    pub resource_href: Option<String>,
}

#[derive(Deserialize)]
pub struct EwsItemRow {
    pub server_id: String,
    pub resource_href: String,
    pub uid: Option<String>,
    pub etag: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
struct EwsSyncStateRow {
    sync_state: String,
}

impl Storage {
    pub fn new(worker_url: &str, worker_secret: &str) -> Result<Self> {
        let retry_policy = ExponentialBackoff::builder()
            .retry_bounds(Duration::from_millis(50), Duration::from_secs(3))
            .build_with_max_retries(3);

        let client = ClientBuilder::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(15))
                .pool_idle_timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(4)
                .build()?,
        )
        .with(TracingMiddleware::default())
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build();

        Ok(Self {
            client,
            base_url: worker_url.trim_end_matches('/').to_string(),
            secret: worker_secret.to_string(),
        })
    }

    fn make_idempotency_key<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update(b"\n");
        let payload = serde_json::to_vec(body)?;
        hasher.update(&payload);
        let digest = hasher.finalize();
        Ok(digest.iter().map(|b| format!("{:02x}", b)).collect())
    }

    async fn post_json<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> Result<()> {
        let clean_path = path.trim_start_matches('/');
        let url = format!("{}/{}", self.base_url, clean_path);
        let idempotency_key = self.make_idempotency_key(clean_path, body)?;
        let json_body = serde_json::to_string(body)?;
        let resp = self
            .client
            .post(&url)
            .header("x-gateway-secret", &self.secret)
            .header("Idempotency-Key", idempotency_key)
            .header("Content-Type", "application/json")
            .body(json_body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("worker POST {} returned {}", url, resp.status()));
        }
        Ok(())
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("x-gateway-secret", &self.secret)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("worker GET {} returned {}", url, resp.status()));
        }
        Ok(resp.json::<T>().await?)
    }

    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        let body = SetSyncKeyReq {
            owner,
            collection_id,
            sync_key,
            token,
        };
        self.post_json("set_sync_key", &body).await
    }

    pub async fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        let path = format!(
            "get_sync_key?owner={}&collection_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(collection_id)
        );
        let row: Option<SyncKeyRow> = self.get_json(&path).await?;
        Ok(row.map(|r| (r.sync_key, r.token)))
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
        let body = UpsertItemReq {
            owner,
            caldav_href,
            resource_href,
            server_id,
            uid,
            etag,
        };
        self.post_json("upsert_item_map", &body).await
    }

    pub async fn get_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        let path = format!(
            "get_client_sync_command?owner={}&collection_id={}&client_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(collection_id),
            urlencoding::encode(client_id)
        );
        let row: Option<ClientSyncCommandRow> = self.get_json(&path).await?;
        Ok(row.map(|r| (r.server_id, r.status)))
    }

    pub async fn put_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
        server_id: Option<&str>,
        status: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            collection_id: &'a str,
            client_id: &'a str,
            server_id: Option<&'a str>,
            status: &'a str,
        }
        self.post_json(
            "put_client_sync_command",
            &Req {
                owner,
                collection_id,
                client_id,
                server_id,
                status,
            },
        )
        .await
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            server_id: &'a str,
        }
        self.post_json("delete_item_by_server_id", &Req { owner, server_id })
            .await
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            server_id: &'a str,
        }
        self.post_json("add_delete_tombstone", &Req { owner, server_id })
            .await
    }

    pub async fn list_changes_since(
        &self,
        owner: &str,
        since_unix_ts: i64,
    ) -> Result<Vec<(String, String)>> {
        let path = format!(
            "list_changes_since?owner={}&since={}",
            urlencoding::encode(owner),
            since_unix_ts
        );
        let rows: Vec<ChangeRow> = self.get_json(&path).await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.server_id, r.resource_href.unwrap_or_default()))
            .collect())
    }

    pub async fn list_deleted_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<String>> {
        let path = format!(
            "list_deleted_since?owner={}&since={}",
            urlencoding::encode(owner),
            since_unix_ts
        );
        let rows: Vec<TombstoneRow> = self.get_json(&path).await?;
        Ok(rows.into_iter().map(|r| r.server_id).collect())
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        let row: LatestSeqRow = self.get_json("get_latest_change_seq").await?;
        Ok(row.seq)
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, String, Option<String>)>> {
        let path = format!(
            "list_changes_since_seq?owner={}&since={}",
            urlencoding::encode(owner),
            since_seq
        );
        let rows: Vec<ChangeRow> = self.get_json(&path).await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.seq.unwrap_or(0), r.server_id, r.resource_href))
            .collect())
    }

    pub async fn list_journal_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
        until_seq: i64,
        limit: usize,
    ) -> Result<Vec<JournalRow>> {
        let path = format!(
            "list_journal_since_seq?owner={}&since={}&until={}&limit={}",
            urlencoding::encode(owner),
            since_seq,
            until_seq,
            limit
        );
        self.get_json(&path).await
    }

    pub async fn list_deleted_since_seq(
        &self,
        owner: &str,
        since_seq: i64,
    ) -> Result<Vec<(i64, String)>> {
        let path = format!(
            "list_deleted_since_seq?owner={}&since={}",
            urlencoding::encode(owner),
            since_seq
        );
        let rows: Vec<TombstoneRow> = self.get_json(&path).await?;
        Ok(rows
            .into_iter()
            .map(|r| (r.seq.unwrap_or(0), r.server_id))
            .collect())
    }

    pub async fn set_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
        policy_key: &str,
        policy_status: &str,
    ) -> Result<()> {
        let body = SetProvisionReq {
            owner,
            device_id,
            policy_key,
            policy_status,
        };
        self.post_json("set_provision_policy", &body).await
    }

    pub async fn get_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<(String, String)>> {
        let path = format!(
            "get_provision_policy?owner={}&device_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(device_id)
        );
        let row: Option<ProvisionRow> = self.get_json(&path).await?;
        Ok(row.map(|r| (r.policy_key, r.policy_status)))
    }

    pub async fn upsert_device_info(
        &self,
        owner: &str,
        device_id: &str,
        friendly_name: &str,
        model: &str,
        os: &str,
        phone_number: &str,
        imei: &str,
        user_agent: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            device_id: &'a str,
            friendly_name: &'a str,
            model: &'a str,
            os: &'a str,
            phone_number: &'a str,
            imei: &'a str,
            user_agent: &'a str,
        }
        self.post_json(
            "upsert_device_info",
            &Req {
                owner,
                device_id,
                friendly_name,
                model,
                os,
                phone_number,
                imei,
                user_agent,
            },
        )
        .await
    }

    pub async fn list_ews_items(
        &self,
        owner: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EwsItemRow>> {
        let path = format!(
            "list_ews_items?owner={}&limit={}&offset={}",
            urlencoding::encode(owner),
            limit,
            offset
        );
        self.get_json(&path).await
    }

    pub async fn get_ews_sync_state(&self, owner: &str, folder_id: &str) -> Result<Option<String>> {
        let path = format!(
            "get_ews_sync_state?owner={}&folder_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(folder_id)
        );
        let row: Option<EwsSyncStateRow> = self.get_json(&path).await?;
        Ok(row.map(|r| r.sync_state))
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        let path = format!(
            "get_ews_item_by_id?owner={}&server_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(server_id)
        );
        self.get_json(&path).await
    }

    // Look up the owner of an item by server_id (for delegate access)
    pub async fn get_ews_item_owner(&self, server_id: &str) -> Result<Option<String>> {
        #[derive(Deserialize)]
        struct OwnerRow {
            owner: String,
        }
        let path = format!("get_ews_item_owner?server_id={}", urlencoding::encode(server_id));
        let row: Option<OwnerRow> = self.get_json(&path).await?;
        Ok(row.map(|r| r.owner))
    }

    pub async fn set_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
        sync_state: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            folder_id: &'a str,
            sync_state: &'a str,
        }
        self.post_json(
            "set_ews_sync_state",
            &Req {
                owner,
                folder_id,
                sync_state,
            },
        )
        .await
    }

    pub async fn upsert_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
        server_id: Option<&str>,
        is_deleted: bool,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            parent_server_id: &'a str,
            exception_start: &'a str,
            server_id: Option<&'a str>,
            is_deleted: i32,
        }
        self.post_json(
            "upsert_calendar_exception",
            &Req {
                owner,
                parent_server_id,
                exception_start,
                server_id,
                is_deleted: if is_deleted { 1 } else { 0 },
            },
        )
        .await
    }

    pub async fn get_calendar_exceptions(
        &self,
        owner: &str,
        parent_server_id: &str,
    ) -> Result<Vec<CalendarExceptionRow>> {
        let path = format!(
            "get_calendar_exceptions?owner={}&parent_server_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(parent_server_id)
        );
        self.get_json(&path).await
    }

    pub async fn get_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<Option<CalendarExceptionRow>> {
        let path = format!(
            "get_calendar_exception?owner={}&parent_server_id={}&exception_start={}",
            urlencoding::encode(owner),
            urlencoding::encode(parent_server_id),
            urlencoding::encode(exception_start)
        );
        self.get_json(&path).await
    }

    pub async fn delete_calendar_exception(
        &self,
        owner: &str,
        parent_server_id: &str,
        exception_start: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            parent_server_id: &'a str,
            exception_start: &'a str,
        }
        self.post_json(
            "delete_calendar_exception",
            &Req {
                owner,
                parent_server_id,
                exception_start,
            },
        )
        .await
    }

    pub async fn record_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
        calendar_id: &str,
        user_response: i32,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            request_id: &'a str,
            calendar_id: &'a str,
            user_response: i32,
        }
        self.post_json(
            "record_meeting_response",
            &Req {
                owner,
                request_id,
                calendar_id,
                user_response,
            },
        )
        .await
    }

    pub async fn upsert_meeting_state(
        &self,
        owner: &str,
        uid: &str,
        sequence: u32,
        state: &str,
        state_flags: u8,
        is_organizer: bool,
        organizer_email: Option<&str>,
        organizer_name: Option<&str>,
        subject: Option<&str>,
        location: Option<&str>,
        start_time: &str,
        end_time: &str,
        timezone: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            uid: &'a str,
            sequence: u32,
            state: &'a str,
            state_flags: u8,
            is_organizer: i32,
            organizer_email: Option<&'a str>,
            organizer_name: Option<&'a str>,
            subject: Option<&'a str>,
            location: Option<&'a str>,
            start_time: &'a str,
            end_time: &'a str,
            timezone: Option<&'a str>,
        }
        self.post_json(
            "upsert_meeting_state",
            &Req {
                owner,
                uid,
                sequence,
                state,
                state_flags,
                is_organizer: if is_organizer { 1 } else { 0 },
                organizer_email,
                organizer_name,
                subject,
                location,
                start_time,
                end_time,
                timezone,
            },
        )
        .await
    }

    pub async fn get_meeting_state(
        &self,
        owner: &str,
        uid: &str,
    ) -> Result<Option<MeetingStateRow>> {
        let path = format!(
            "get_meeting_state?owner={}&uid={}",
            urlencoding::encode(owner),
            urlencoding::encode(uid)
        );
        self.get_json(&path).await
    }

    pub async fn delete_meeting_state(&self, owner: &str, uid: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            uid: &'a str,
        }
        self.post_json("delete_meeting_state", &Req { owner, uid }).await
    }

    pub async fn upsert_meeting_attendee(
        &self,
        owner: &str,
        meeting_uid: &str,
        email: &str,
        name: Option<&str>,
        status: u8,
        role: u8,
        response_time: Option<&str>,
        proposed_start: Option<&str>,
        proposed_end: Option<&str>,
        sequence: u32,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            meeting_uid: &'a str,
            email: &'a str,
            name: Option<&'a str>,
            status: u8,
            role: u8,
            response_time: Option<&'a str>,
            proposed_start: Option<&'a str>,
            proposed_end: Option<&'a str>,
            sequence: u32,
        }
        self.post_json(
            "upsert_meeting_attendee",
            &Req {
                owner,
                meeting_uid,
                email,
                name,
                status,
                role,
                response_time,
                proposed_start,
                proposed_end,
                sequence,
            },
        )
        .await
    }

    pub async fn get_meeting_attendees(
        &self,
        owner: &str,
        meeting_uid: &str,
    ) -> Result<Vec<MeetingAttendeeRow>> {
        let path = format!(
            "get_meeting_attendees?owner={}&meeting_uid={}",
            urlencoding::encode(owner),
            urlencoding::encode(meeting_uid)
        );
        self.get_json(&path).await
    }

    pub async fn delete_meeting_attendee(
        &self,
        owner: &str,
        meeting_uid: &str,
        email: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            meeting_uid: &'a str,
            email: &'a str,
        }
        self.post_json(
            "delete_meeting_attendee",
            &Req {
                owner,
                meeting_uid,
                email,
            },
        )
        .await
    }

    pub async fn delete_meeting_attendees(
        &self,
        owner: &str,
        meeting_uid: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            meeting_uid: &'a str,
        }
        self.post_json(
            "delete_meeting_attendees",
            &Req { owner, meeting_uid },
        )
        .await
    }

    pub async fn enqueue_scheduling(
        &self,
        owner: &str,
        meeting_uid: &str,
        operation: &str,
        sequence: u32,
        ical_data: &str,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            owner: &'a str,
            meeting_uid: &'a str,
            operation: &'a str,
            sequence: u32,
            ical_data: &'a str,
        }
        self.post_json(
            "enqueue_scheduling",
            &Req {
                owner,
                meeting_uid,
                operation,
                sequence,
                ical_data,
            },
        )
        .await
    }

    pub async fn get_pending_scheduling(
        &self,
        owner: &str,
        limit: usize,
    ) -> Result<Vec<SchedulingQueueRow>> {
        let path = format!(
            "get_pending_scheduling?owner={}&limit={}",
            urlencoding::encode(owner),
            limit
        );
        self.get_json(&path).await
    }

    pub async fn mark_scheduling_processed(
        &self,
        id: i64,
        status: &str,
        error_message: Option<&str>,
    ) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            id: i64,
            status: &'a str,
            error_message: Option<&'a str>,
        }
        self.post_json(
            "mark_scheduling_processed",
            &Req {
                id,
                status,
                error_message,
            },
        )
        .await
    }

    pub async fn get_meetings_by_time_range(
        &self,
        owner: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<MeetingStateRow>> {
        let path = format!(
            "get_meetings_by_time_range?owner={}&start={}&end={}",
            urlencoding::encode(owner),
            urlencoding::encode(start),
            urlencoding::encode(end)
        );
        self.get_json(&path).await
    }

    pub async fn get_meeting_response(
        &self,
        owner: &str,
        request_id: &str,
    ) -> Result<Option<MeetingResponseRow>> {
        let path = format!(
            "get_meeting_response?owner={}&request_id={}",
            urlencoding::encode(owner),
            urlencoding::encode(request_id)
        );
        self.get_json(&path).await
    }
}

#[derive(Deserialize)]
pub struct CalendarExceptionRow {
    pub parent_server_id: String,
    pub exception_start: String,
    pub server_id: Option<String>,
    pub is_deleted: i32,
    pub created_at: String,
}

#[derive(Deserialize)]
pub struct MeetingResponseRow {
    pub request_id: String,
    pub calendar_id: String,
    pub user_response: i32,
    pub created_at: String,
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
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

#[derive(Deserialize)]
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
