use crate::config::Config;
use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Serialize, Deserialize};

#[derive(Clone)]
pub struct Storage {
    client: Client,
    base_url: String,
    secret: String,
}

impl Storage {
    /// Initialize Cloudflare-backed storage using the Worker API.
    pub fn new(cfg: &Config) -> Self {
        Storage {
            client: Client::new(),
            base_url: cfg.worker_url.clone(),
            secret: cfg.worker_secret.clone(),
        }
    }

    /// Retrieve the current SyncKey for the owner/collection.
    pub async fn get_sync_key(&self, owner: &str, collection_id: &str) -> Result<Option<String>> {
        let url = format!("{}/sync-key?owner={}&collection_id={}", self.base_url, owner, collection_id);
        let resp = self.client
            .get(&url)
            .header("Authorization", &self.secret)
            .send()
            .await?;
        if resp.status().is_success() {
            #[derive(Deserialize)] struct SyncKeyResp { sync_key: Option<String> }
            let data: SyncKeyResp = resp.json().await?;
            Ok(data.sync_key)
        } else {
            Ok(None)
        }
    }

    /// Set or update the SyncKey for the owner/collection.
    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        let url = format!("{}/sync-key", self.base_url);
        #[derive(Serialize)] struct Req<'a> { owner: &'a str, collection_id: &'a str, sync_key: &'a str, token: Option<&'a str> }
        let body = Req { owner, collection_id, sync_key, token };
        let resp = self.client
            .post(&url)
            .header("Authorization", &self.secret)
            .json(&body)
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("set_sync_key failed: {}", resp.status()))
        }
    }

    /// Map or update an item: owner, CalDAV href, resource URL, server_id, uid, etag.
    pub async fn upsert_item_map(
        &self,
        owner: &str,
        caldav_href: &str,
        resource_href: &str,
        server_id: &str,
        uid: &str,
        etag: &str,
    ) -> Result<()> {
        let url = format!("{}/item", self.base_url);
        #[derive(Serialize)] struct Item<'a> {
            owner: &'a str,
            caldav_href: &'a str,
            resource_href: &'a str,
            server_id: &'a str,
            uid: &'a str,
            etag: &'a str,
        }
        let body = Item { owner, caldav_href, resource_href, server_id, uid, etag };
        let resp = self.client
            .post(&url)
            .header("Authorization", &self.secret)
            .json(&body)
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("upsert_item_map failed: {}", resp.status()))
        }
    }

    /// Lookup an item by its server_id (returns owner, resource URL).
    pub async fn get_item_by_server_id(&self, server_id: &str) -> Result<Option<(String, String)>> {
        let url = format!("{}/item?server_id={}", self.base_url, server_id);
        let resp = self.client
            .get(&url)
            .header("Authorization", &self.secret)
            .send()
            .await?;
        if resp.status().is_success() {
            #[derive(Deserialize)] struct ItemResp { owner: String, resource_href: String }
            let data: ItemResp = resp.json().await?;
            Ok(Some((data.owner, data.resource_href)))
        } else {
            Ok(None)
        }
    }

    /// Delete an item mapping by its server_id.
    pub async fn delete_item_by_server_id(&self, server_id: &str) -> Result<()> {
        let url = format!("{}/item?server_id={}", self.base_url, server_id);
        let resp = self.client
            .delete(&url)
            .header("Authorization", &self.secret)
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(anyhow!("delete_item_by_server_id failed: {}", resp.status()))
        }
    }

    /// List all items changed since a given Unix timestamp (seconds).
    pub async fn list_changes_since(&self, owner: &str, since_unix_ts: i64) -> Result<Vec<(String, String)>> {
        let url = format!("{}/changes?owner={}&since={}", self.base_url, owner, since_unix_ts);
        let resp = self.client
            .get(&url)
            .header("Authorization", &self.secret)
            .send()
            .await?;
        if resp.status().is_success() {
            #[derive(Deserialize)] struct Change { server_id: String, resource_href: String }
            #[derive(Deserialize)] struct ChangesResp { changes: Vec<Change> }
            let data: ChangesResp = resp.json().await?;
            Ok(data.changes.into_iter().map(|c| (c.server_id, c.resource_href)).collect())
        } else {
            Err(anyhow!("list_changes_since failed: {}", resp.status()))
        }
    }
}
