// src/storage.rs
use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Clone)]
pub struct Storage {
    client: Client,
    base_url: String, // e.g. "https://exchange.mail.example.com/api"
    secret: String,   // worker secret header value
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
    /// Construct a Storage client that talks to the Cloudflare Worker endpoint.
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

    fn auth_header_value(&self) -> String {
        self.secret.clone()
    }

    async fn post_json<T: Serialize + ?Sized, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-gateway-secret", self.auth_header_value())
            .json(body)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("worker POST {} returned {}", url, resp.status()));
        }
        let v = resp.json::<R>().await?;
        Ok(v)
    }

    async fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T> {
        let url = format!("{}/{}", self.base_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("x-gateway-secret", self.auth_header_value())
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("worker GET {} returned {}", url, resp.status()));
        }
        let v = resp.json::<T>().await?;
        Ok(v)
    }

    /// Set sync key (upsert)
    pub async fn set_sync_key(&self, owner: &str, collection_id: &str, sync_key: &str, token: Option<&str>) -> Result<()> {
        let body = SetSyncKeyReq { owner, collection_id, sync_key, token };
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkResp { ok: bool }
        let _r: OkResp = self.post_json("set_sync_key", &body).await?;
        Ok(())
    }

    /// Upsert an item mapping (owner, caldav_href, resource_href, server_id, uid, etag)
    pub async fn upsert_item_map(&self, owner: &str, caldav_href: &str, resource_href: &str, server_id: &str, uid: &str, etag: &str) -> Result<()> {
        let body = UpsertItemReq { owner, caldav_href, resource_href, server_id, uid, etag };
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkResp { ok: bool }
        let _r: OkResp = self.post_json("upsert_item_map", &body).await?;
        Ok(())
    }

    /// Delete item mapping by server_id
    pub async fn delete_item_by_server_id(&self, server_id: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Req<'a> { server_id: &'a str }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkResp { ok: bool }
        let _r: OkResp = self.post_json("delete_item_by_server_id", &Req { server_id }).await?;
        Ok(())
    }

    /// List changes since unix timestamp (seconds) for owner -> Vec<(server_id, resource_href)>
    pub async fn list_changes_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<(String, String)>> {
        let path = format!("list_changes_since?owner={}&since={}", urlencoding::encode(owner), since_unix_ts);
        let rows: Vec<ChangeRow> = self.get_json(&path).await?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            out.push((r.server_id, r.resource_href));
        }
        Ok(out)
    }
}
