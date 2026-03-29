// src/storage.rs
use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Clone)]
pub struct Storage {
    client: Client,
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

#[derive(Clone, Deserialize)]
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
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(15))
            .build()?;
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
        let resp = self
            .client
            .post(&url)
            .header("x-gateway-secret", &self.secret)
            .header("Idempotency-Key", idempotency_key)
            .json(body)
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
        let v = resp.json::<T>().await?;
        Ok(v)
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
        let req = Req {
            owner,
            folder_id,
            sync_state,
        };
        self.post_json("set_ews_sync_state", &req).await
    }
}
