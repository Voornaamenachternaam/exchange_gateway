use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub bind: String,
    pub caldav_base: String,
    pub worker_url: String,
    pub worker_secret: String,
    pub hmac_secret: String,
}

impl Config {
    pub fn from_env_or_file() -> Result<Self> {
        if let Ok(cfg) = Self::from_env() {
            return Ok(cfg);
        }
        Self::from_file("/etc/exchange-gateway/config.toml")
    }

    fn from_env() -> Result<Self> {
        Ok(Config {
            bind: std::env::var("GATEWAY_BIND").unwrap_or_else(|_| "0.0.0.0:8134".to_string()),
            caldav_base: std::env::var("CALDAV_BASE")
                .context("CALDAV_BASE environment variable not set")?,
            worker_url: std::env::var("WORKER_URL")
                .context("WORKER_URL environment variable not set")?,
            worker_secret: std::env::var("WORKER_SECRET")
                .context("WORKER_SECRET environment variable not set")?,
            hmac_secret: std::env::var("HMAC_SECRET")
                .context("HMAC_SECRET environment variable not set")?,
        })
    }

    fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let cfg: Config = toml::from_str(&content)?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_parses_from_valid_toml() {
        let toml = r#"
            bind = "0.0.0.0:8134"
            caldav_base = "http://localhost:8080/dav/"
            worker_url = "https://worker.example.com/api"
            worker_secret = "secret123"
            hmac_secret = "hmacsecret456"
        "#;
        let cfg: Config = toml::from_str(toml).unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:8134");
        assert_eq!(cfg.caldav_base, "http://localhost:8080/dav/");
    }
}
