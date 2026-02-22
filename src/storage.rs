// src/storage.rs
use anyhow::Result;
use reqwest::Client;
use std::sync::Arc;

/// Minimal Storage implementation that uses your Cloudflare Worker for
/// any storage/state interactions. This is intentionally simple: it
/// provides an HTTP client preconfigured with the worker endpoint + secret.
#[derive(Clone)]
pub struct Storage {
    client: Arc<Client>,
    pub worker_url: String,
    pub worker_secret: String,
}

impl Storage {
    /// Construct a new Storage instance using worker endpoint and secret.
    /// This will return an error if the endpoint is invalid, or the client cannot be created.
    pub fn new(worker_url: &str, worker_secret: &str) -> Result<Self> {
        let client = Client::builder().build()?;
        Ok(Self {
            client: Arc::new(client),
            worker_url: worker_url.to_string(),
            worker_secret: worker_secret.to_string(),
        })
    }

    /// Example helper: POST JSON to worker /put with a key and json value
    /// (not used heavily by the minimal gateway but provided for future use).
    pub async fn put_json(&self, key: &str, json_body: &serde_json::Value) -> Result<()> {
        let url = format!("{}/put", self.worker_url.trim_end_matches('/'));
        let res = self
            .client
            .post(&url)
            .header("X-GATEWAY-SECRET", &self.worker_secret)
            .json(&serde_json::json!({"key": key, "value": json_body}))
            .send()
            .await?;
        if !res.status().is_success() {
            let s = res.text().await.unwrap_or_default();
            anyhow::bail!("worker put failed: {} {}", res.status(), s);
        }
        Ok(())
    }

    /// Example helper: GET JSON by key
    pub async fn get_json(&self, key: &str) -> Result<Option<serde_json::Value>> {
        let url = format!("{}/get/{}", self.worker_url.trim_end_matches('/'), urlencoding::encode(key));
        let res = self
            .client
            .get(&url)
            .header("X-GATEWAY-SECRET", &self.worker_secret)
            .send()
            .await?;
        if res.status().is_success() {
            let v: serde_json::Value = res.json().await?;
            Ok(Some(v))
        } else if res.status().as_u16() == 404 {
            Ok(None)
        } else {
            let s = res.text().await.unwrap_or_default();
            anyhow::bail!("worker get failed: {} {}", res.status(), s);
        }
    }
}
