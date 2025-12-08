use serde::Deserialize;
use std::fs;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind: String,
    pub http_bind: String,
    pub tls_cert: String,
    pub tls_key: String,
    pub caldav_base: String,
    pub db_path: String,
    pub hmac_secret: String,
    pub log_level: Option<String>,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let s = fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&s)?;
        Ok(cfg)
    }
}
