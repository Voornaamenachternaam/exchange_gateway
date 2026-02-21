use anyhow::Result;
use serde::{Deserialize, Serialize};
use reqwest::Client;
use std::time::Duration;

#[derive(Clone)]
pub struct Storage {
    client: Client,
    worker_url: String,
    secret: String,
}

#[derive(Serialize)]
struct SetSyncKeyBody<'a> {
    owner: &'a str,
    collection_id: &'a str,
    sync_key: &'a str,
    token: Option<&'a str>,
}

#[derive(Deserialize)]
struct GetSyncKeyResp {
    sync_key: Option<String>,
}

#[derive(Serialize)]
struct UpsertItemBody<'a> {
    owner: &'a str,
    caldav_href: &'a str,
    resource_href: &'a str,
    server_id: &'a str,
    uid: &'a str,
    etag: &'a str,
}

#[derive(Deserialize)]
struct ItemByServerIdResp {
    found: bool,
    id: Option<i64>,
    resource_href: Option<String>,
}

impl Storage {
    /// Construct Storage that calls the configured Worker endpoint.
    pub async fn new(worker_url: &str, secret: &str) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            worker_url: worker_url.trim_end_matches('/').to_string(),
            secret: secret.to_string(),
        })
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response> {
        let url = format!("{}/{}", self.worker_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .get(&url)
            .header("x-gateway-secret", &self.secret)
            .send()
            .await?;
        Ok(resp)
    }

    async fn post<T: Serialize + ?Sized>(&self, path: &str, body: &T) -> Result<reqwest::Response> {
        let url = format!("{}/{}", self.worker_url, path.trim_start_matches('/'));
        let resp = self
            .client
            .post(&url)
            .header("x-gateway-secret", &self.secret)
            .json(body)
            .send()
            .await?;
        Ok(resp)
    }

    pub async fn get_sync_key(&self, owner: &str, collection_id: &str) -> Result<Option<String>> {
        let url_path = format!("get_sync_key?owner={}&collection_id={}", urlencoding::encode(owner), urlencoding::encode(collection_id));
        let resp = self.get(&url_path).await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("worker get_sync_key failed: {}", resp.status()));
        }
        let g: GetSyncKeyResp = resp.json().await?;
        Ok(g.sync_key)
    }

    pub async fn set_sync_key(&self, owner: &str, collection_id: &str, sync_key: &str, token: Option<&str>) -> Result<()> {
        let body = SetSyncKeyBody { owner, collection_id, sync_key, token };
        let resp = self.post("set_sync_key", &body).await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("worker set_sync_key failed: {}", resp.status()))
        }
    }

    pub async fn upsert_item_map(&self, owner: &str, caldav_href: &str, resource_href: &str, server_id: &str, uid: &str, etag: &str) -> Result<()> {
        let body = UpsertItemBody { owner, caldav_href, resource_href, server_id, uid, etag };
        let resp = self.post("upsert_item_map", &body).await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("worker upsert_item_map failed: {}", resp.status()))
        }
    }

    /// Returns Option<(id, resource_href)>
    pub async fn get_item_by_server_id(&self, server_id: &str) -> Result<Option<(i64, String)>> {
        let path = format!("get_item_by_server_id?server_id={}", urlencoding::encode(server_id));
        let resp = self.get(&path).await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("worker get_item_by_server_id failed: {}", resp.status()));
        }
        let r: ItemByServerIdResp = resp.json().await?;
        if r.found {
            Ok(Some((r.id.unwrap_or_default(), r.resource_href.unwrap_or_default())))
        } else {
            Ok(None)
        }
    }

    pub async fn delete_item_by_server_id(&self, server_id: &str) -> Result<()> {
        let body = serde_json::json!({ "server_id": server_id });
        let resp = self.post("delete_item_by_server_id", &body).await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow::anyhow!("worker delete_item_by_server_id failed: {}", resp.status()))
        }
    }

    pub async fn list_changes_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<(String, String)>> {
        let path = format!("list_changes_since?owner={}&since={}", urlencoding::encode(owner), since_unix_ts);
        let resp = self.get(&path).await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("worker list_changes_since failed: {}", resp.status()));
        }
        let rows: Vec<serde_json::Value> = resp.json().await?;
        let mut res = Vec::new();
        for v in rows {
            let server_id = v.get("server_id").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let resource_href = v.get("resource_href").and_then(|s| s.as_str()).unwrap_or("").to_string();
            if !server_id.is_empty() {
                res.push((server_id, resource_href));
            }
        }
        Ok(res)
    }
}
