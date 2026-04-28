// src/config.rs
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::fs;
use url::Url;
use zeroize::Zeroizing;

const DEFAULT_MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_AUTH_CACHE_TTL_SECS: u64 = 300;
const DEFAULT_AUTH_CACHE_MAX_ENTRIES: usize = 10000;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub gateway_host: String,
    #[serde(default)]
    pub mail_domain: String,
    pub bind: String,
    pub caldav_base: String,
    pub database_path: String,
    #[serde(skip_serializing)]
    pub hmac_secret: SecretString,
    #[serde(default = "default_max_attachment_bytes")]
    pub max_attachment_bytes: usize,
    #[serde(default)]
    pub room_booking_enabled: bool,
    #[serde(default = "default_auth_cache_ttl_secs")]
    pub auth_cache_ttl_secs: u64,
    #[serde(default = "default_auth_cache_max_entries")]
    pub auth_cache_max_entries: usize,
}

fn default_max_attachment_bytes() -> usize {
    DEFAULT_MAX_ATTACHMENT_BYTES
}

fn default_auth_cache_ttl_secs() -> u64 {
    DEFAULT_AUTH_CACHE_TTL_SECS
}

fn default_auth_cache_max_entries() -> usize {
    DEFAULT_AUTH_CACHE_MAX_ENTRIES
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content: Zeroizing<String> = fs::read_to_string(path)
            .map(Zeroizing::new)
            .map_err(|e| anyhow::anyhow!("Cannot read config file at '{}': {}", path, e))?;
        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML: {}", e))?;
        cfg.gateway_host = cfg.gateway_host.trim().to_string();
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn hmac_secret(&self) -> &str {
        self.hmac_secret.expose_secret()
    }

    pub fn max_attachment_bytes(&self) -> usize {
        self.max_attachment_bytes.max(1024)
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.bind.is_empty() {
            return Err(anyhow::anyhow!("Config: 'bind' address is required"));
        }
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config: 'bind' must be in format 'host:port'"
            ));
        }
        if self.mail_domain.trim().is_empty() {
            return Err(anyhow::anyhow!("Config: 'mail_domain' is required"));
        }
        if self.gateway_host.trim().is_empty() {
            return Err(anyhow::anyhow!("Config: 'gateway_host' is required"));
        }
        validate_url(&self.caldav_base, "caldav_base")?;
        if self.hmac_secret.expose_secret().len() < 32 {
            return Err(anyhow::anyhow!(
                "Config: 'hmac_secret' must be at least 32 characters"
            ));
        }
        if self.database_path.trim().is_empty() {
            return Err(anyhow::anyhow!("Config: 'database_path' is required"));
        }
        if self.hmac_secret.expose_secret().starts_with("REPLACE_") {
            return Err(anyhow::anyhow!(
                "Config: 'hmac_secret' still contains a placeholder"
            ));
        }
        if !self.gateway_host.is_empty() && self.gateway_host.contains("://") {
            return Err(anyhow::anyhow!(
                "Config: 'gateway_host' must be a hostname only, not a URL"
            ));
        }
        if self.max_attachment_bytes > 50 * 1024 * 1024 {
            return Err(anyhow::anyhow!(
                "Config: 'max_attachment_bytes' must not exceed 50MB"
            ));
        }
        Ok(())
    }
}

fn validate_url(url: &str, field_name: &str) -> anyhow::Result<()> {
    match Url::parse(url) {
        Ok(parsed) => {
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(anyhow::anyhow!(
                    "Config: '{}' must use http or https scheme",
                    field_name
                ));
            }
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "Config: '{}' is not a valid URL: {}",
            field_name,
            e
        )),
    }
}
