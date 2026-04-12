// src/config.rs
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::fs;
use url::Url;
use zeroize::Zeroizing;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind: String,
    pub caldav_base: String,
    pub worker_url: String,
    pub worker_secret: SecretString,
    pub hmac_secret: SecretString,

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

        cfg.validate()?;

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

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.bind.is_empty() {
            return Err(anyhow::anyhow!("Config: 'bind' address is required"));
        }
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config: 'bind' must be in format 'host:port'"
            ));
        }
        if self.caldav_base.is_empty() {
            return Err(anyhow::anyhow!("Config: 'caldav_base' URL is required"));
        }
        validate_url(&self.caldav_base, "caldav_base")?;

        if self.worker_url.is_empty() {
            return Err(anyhow::anyhow!("Config: 'worker_url' is required"));
        }
        validate_url(&self.worker_url, "worker_url")?;

        if self.worker_secret.expose_secret().is_empty() {
            return Err(anyhow::anyhow!("Config: 'worker_secret' is required"));
        }
        if self.worker_secret.expose_secret().len() < 16 {
            tracing::warn!(
                "Config: worker_secret is shorter than 16 characters — this is insecure"
            );
        }

        if self.hmac_secret.expose_secret().is_empty() {
            return Err(anyhow::anyhow!("Config: 'hmac_secret' is required"));
        }
        if self.hmac_secret.expose_secret().len() < 32 {
            tracing::warn!("Config: hmac_secret is shorter than 32 characters — this is insecure");
        }

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
