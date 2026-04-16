// src/config.rs
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::fs;
use url::Url;
use validator::{Validate, ValidationError};
use zeroize::Zeroizing;

#[derive(Clone, Debug, Deserialize, Validate)]
pub struct Config {
    #[validate(length(min = 1, message = "bind address is required"))]
    pub bind: String,

    #[validate(url(message = "caldav_base must be a valid URL"))]
    pub caldav_base: String,

    #[validate(url(message = "worker_url must be a valid URL"))]
    pub worker_url: String,

    #[validate(length(min = 16, message = "worker_secret must be at least 16 characters"))]
    #[serde(skip_serializing)]
    pub worker_secret: SecretString,

    #[validate(length(min = 32, message = "hmac_secret must be at least 32 characters"))]
    #[serde(skip_serializing)]
    pub hmac_secret: SecretString,

    #[validate(length(max = 0, message = "gateway_host must not contain protocol"))]
    #[serde(default)]
    pub gateway_host: String,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content: Zeroizing<String> = fs::read_to_string(path)
            .map(Zeroizing::new)
            .map_err(|e| anyhow::anyhow!("Cannot read config file at '{}': {}", path, e))?;

        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML: {}", e))?;

        // Use validator for automatic validation
        cfg.validate()
            .map_err(|e| anyhow::anyhow!("Config validation failed: {}", e))?;

        cfg.validate_custom()?;

        if cfg.gateway_host.is_empty() {
            cfg.gateway_host =
                extract_host_from_url(&cfg.worker_url).unwrap_or_else(|| "localhost".to_string());
        }

        Ok(cfg)
    }

    pub fn worker_secret(&self) -> &str {
        self.worker_secret.expose_secret()
    }

    pub fn hmac_secret(&self) -> &str {
        self.hmac_secret.expose_secret()
    }

    /// Custom validation that can't be expressed with validator derive
    fn validate_custom(&self) -> anyhow::Result<()> {
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config: 'bind' must be in format 'host:port'"
            ));
        }

        validate_url(&self.caldav_base, "caldav_base")?;
        validate_url(&self.worker_url, "worker_url")?;

        if !self.gateway_host.is_empty() && self.gateway_host.contains("://") {
            return Err(anyhow::anyhow!(
                "Config: 'gateway_host' must be a hostname only, not a URL"
            ));
        }

        Ok(())
    }
}

fn extract_host_from_url(url_str: &str) -> Option<String> {
    let parsed = Url::parse(url_str).ok()?;
    let host = parsed.host_str()?;
    match parsed.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host.to_string()),
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
