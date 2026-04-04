use crate::config::Config;
use crate::wbxml::Wbxml;
use anyhow::Result;
use reqwest::Client;
use std::sync::Arc;

pub struct AppState {
    pub cfg: Config,
    pub http: Client,
    pub wbxml: Wbxml,
    pub storage: StorageClient,
}

impl AppState {
    pub async fn new(cfg: Config) -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        let storage = StorageClient::new(&cfg.worker_url, &cfg.worker_secret);
        Ok(Self {
            cfg,
            http,
            wbxml: Wbxml::new(),
            storage,
        })
    }
}

#[derive(Clone)]
pub struct StorageClient {
    base_url: String,
    secret: String,
    http: Client,
}

impl StorageClient {
    pub fn new(base_url: &str, secret: &str) -> Self {
        Self {
            base_url: base_url.to_string(),
            secret: secret.to_string(),
            http: Client::new(),
        }
    }

    fn auth_headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.secret).parse().unwrap(),
        );
        headers.insert("Content-Type", "application/json".parse().unwrap());
        headers
    }

    pub async fn set_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
        sync_key: &str,
        token: Option<&str>,
    ) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "collection_id": collection_id,
            "sync_key": sync_key,
            "token": token,
        });
        self.http
            .post(format!("{}/set_sync_key", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn get_sync_key(
        &self,
        owner: &str,
        collection_id: &str,
    ) -> Result<Option<(String, Option<String>)>> {
        let url = format!(
            "{}/get_sync_key?owner={}&collection_id={}",
            self.base_url,
            urlencoding::encode(owner),
            urlencoding::encode(collection_id)
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            if json.is_null() {
                return Ok(None);
            }
            let sync_key = json["sync_key"].as_str().unwrap_or("").to_string();
            let token = json["token"].as_str().map(|s| s.to_string());
            return Ok(Some((sync_key, token)));
        }
        Ok(None)
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
        let body = serde_json::json!({
            "owner": owner,
            "caldav_href": caldav_href,
            "resource_href": resource_href,
            "server_id": server_id,
            "uid": uid,
            "etag": etag,
        });
        self.http
            .post(format!("{}/upsert_item_map", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn get_ews_item_by_server_id(
        &self,
        owner: &str,
        server_id: &str,
    ) -> Result<Option<EwsItemRow>> {
        let url = format!(
            "{}/get_ews_item_by_id?owner={}&server_id={}",
            self.base_url,
            urlencoding::encode(owner),
            urlencoding::encode(server_id)
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: Option<EwsItemRow> = resp.json().await?;
            return Ok(json);
        }
        Ok(None)
    }

    pub async fn delete_item_by_server_id(&self, owner: &str, server_id: &str) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "server_id": server_id,
        });
        self.http
            .post(format!("{}/delete_item_by_server_id", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn add_delete_tombstone(&self, owner: &str, server_id: &str) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "server_id": server_id,
        });
        self.http
            .post(format!("{}/add_delete_tombstone", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn put_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
        server_id: Option<&str>,
        status: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "collection_id": collection_id,
            "client_id": client_id,
            "server_id": server_id,
            "status": status,
        });
        self.http
            .post(format!("{}/put_client_sync_command", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn get_client_sync_command(
        &self,
        owner: &str,
        collection_id: &str,
        client_id: &str,
    ) -> Result<Option<(Option<String>, String)>> {
        let url = format!(
            "{}/get_client_sync_command?owner={}&collection_id={}&client_id={}",
            self.base_url,
            urlencoding::encode(owner),
            urlencoding::encode(collection_id),
            urlencoding::encode(client_id)
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            if json.is_null() {
                return Ok(None);
            }
            let server_id = json["server_id"].as_str().map(|s| s.to_string());
            let status = json["status"].as_str().unwrap_or("6").to_string();
            return Ok(Some((server_id, status)));
        }
        Ok(None)
    }

    pub async fn get_latest_change_seq(&self) -> Result<i64> {
        let url = format!("{}/get_latest_change_seq", self.base_url);
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            return Ok(json["seq"].as_i64().unwrap_or(0));
        }
        Ok(0)
    }

    pub async fn list_changes_since_seq(
        &self,
        owner: &str,
        since: i64,
    ) -> Result<Vec<(i64, String, Option<String>)>> {
        let url = format!(
            "{}/list_changes_since_seq?owner={}&since={}",
            self.base_url,
            urlencoding::encode(owner),
            since
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let rows: Vec<serde_json::Value> = resp.json().await?;
            return Ok(rows
                .into_iter()
                .map(|r| {
                    (
                        r["seq"].as_i64().unwrap_or(0),
                        r["server_id"].as_str().unwrap_or("").to_string(),
                        r["resource_href"].as_str().map(|s| s.to_string()),
                    )
                })
                .collect());
        }
        Ok(Vec::new())
    }

    pub async fn list_deleted_since_seq(
        &self,
        owner: &str,
        since: i64,
    ) -> Result<Vec<(i64, String)>> {
        let url = format!(
            "{}/list_deleted_since_seq?owner={}&since={}",
            self.base_url,
            urlencoding::encode(owner),
            since
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let rows: Vec<serde_json::Value> = resp.json().await?;
            return Ok(rows
                .into_iter()
                .map(|r| {
                    (
                        r["seq"].as_i64().unwrap_or(0),
                        r["server_id"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect());
        }
        Ok(Vec::new())
    }

    pub async fn set_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
        policy_key: &str,
        policy_status: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "device_id": device_id,
            "policy_key": policy_key,
            "policy_status": policy_status,
        });
        self.http
            .post(format!("{}/set_provision_policy", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn get_provision_policy(
        &self,
        owner: &str,
        device_id: &str,
    ) -> Result<Option<(String, String)>> {
        let url = format!(
            "{}/get_provision_policy?owner={}&device_id={}",
            self.base_url,
            urlencoding::encode(owner),
            urlencoding::encode(device_id)
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            if json.is_null() {
                return Ok(None);
            }
            let policy_key = json["policy_key"].as_str().unwrap_or("").to_string();
            let policy_status = json["policy_status"].as_str().unwrap_or("").to_string();
            return Ok(Some((policy_key, policy_status)));
        }
        Ok(None)
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
        let body = serde_json::json!({
            "owner": owner,
            "device_id": device_id,
            "friendly_name": friendly_name,
            "model": model,
            "os": os,
            "phone_number": phone_number,
            "imei": imei,
            "user_agent": user_agent,
        });
        self.http
            .post(format!("{}/upsert_device_info", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }

    pub async fn list_ews_items(
        &self,
        owner: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<EwsItemRow>> {
        let url = format!(
            "{}/list_ews_items?owner={}&limit={}&offset={}",
            self.base_url,
            urlencoding::encode(owner),
            limit,
            offset
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let rows: Vec<EwsItemRow> = resp.json().await?;
            return Ok(rows);
        }
        Ok(Vec::new())
    }

    pub async fn get_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
    ) -> Result<Option<String>> {
        let url = format!(
            "{}/get_ews_sync_state?owner={}&folder_id={}",
            self.base_url,
            urlencoding::encode(owner),
            urlencoding::encode(folder_id)
        );
        let resp = self
            .http
            .get(&url)
            .headers(self.auth_headers())
            .send()
            .await?;
        if resp.status().is_success() {
            let json: serde_json::Value = resp.json().await?;
            if json.is_null() {
                return Ok(None);
            }
            return Ok(json["sync_state"].as_str().map(|s| s.to_string()));
        }
        Ok(None)
    }

    pub async fn set_ews_sync_state(
        &self,
        owner: &str,
        folder_id: &str,
        sync_state: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "owner": owner,
            "folder_id": folder_id,
            "sync_state": sync_state,
        });
        self.http
            .post(format!("{}/set_ews_sync_state", self.base_url))
            .headers(self.auth_headers())
            .json(&body)
            .send()
            .await?;
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct EwsItemRow {
    pub server_id: String,
    pub resource_href: String,
    pub uid: String,
    pub etag: Option<String>,
}
