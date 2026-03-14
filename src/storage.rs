// src/storage.rs
use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
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

#[derive(Deserialize)]
struct ChangeRow {
    server_id: String,
    resource_href: String,
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

    async fn post_json<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> Result<()> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-gateway-secret", &self.secret)
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

    pub async fn delete_item_by_server_id(&self, server_id: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> {
            server_id: &'a str,
        }
        self.post_json("delete_item_by_server_id", &Req { server_id })
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
            .map(|r| (r.server_id, r.resource_href))
            .collect())
    }
}
