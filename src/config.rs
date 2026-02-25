// src/config.rs
use serde::Deserialize;
use std::fs;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind: String,
    pub caldav_base: String,
    pub worker_url: String,
    pub worker_secret: String,
    pub hmac_secret: String,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let s = fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&s)?;
        Ok(cfg)
    }
}
