// src/config.rs
use serde::Deserialize;
use std::fs;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// Bind address for the gateway, e.g. "0.0.0.0:8133"
    pub bind: String,
    /// Optional HTTP bind for a metrics/admin endpoint (not required)
    pub http_bind: Option<String>,
    /// CalDAV base URL (Stalwart webdav endpoint)
    pub caldav_base: String,
    /// Cloudflare Worker URL (e.g. https://exchange.mail.example.com/api)
    pub worker_url: String,
    /// Secret value that the Rust gateway will send to the worker for auth
    pub worker_secret: String,
    /// HMAC secret used by the gateway internally (optional)
    pub hmac_secret: Option<String>,
    /// logging level
    pub log_level: Option<String>,
}

impl Config {
    /// Load configuration from a TOML file path
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let s = fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&s)?;
        Ok(cfg)
    }
}
