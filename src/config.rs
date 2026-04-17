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
    #[serde(skip_serializing)]
    pub worker_secret: SecretString,
    #[serde(skip_serializing)]
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

        // Provide fallback for gateway_host from worker_url if not specified
        if cfg.gateway_host.is_empty() {
            if let Some(host) = extract_host_from_url(&cfg.worker_url) {
                cfg.gateway_host = host;
            } else {
                tracing::warn!(
                    "Config: 'gateway_host' not specified and could not be extracted from 'worker_url'. \
                    Autodiscover responses may be incorrect."
                );
            }
        }

        cfg.validate()?;

        Ok(cfg)
    }

    pub fn worker_secret(&self) -> &str {
        self.worker_secret.expose_secret()
    }

    pub fn hmac_secret(&self) -> &str {
        self.hmac_secret.expose_secret()
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

        validate_url(&self.caldav_base, "caldav_base")?;
        validate_url(&self.worker_url, "worker_url")?;

        if self.worker_secret.expose_secret().len() < 16 {
            return Err(anyhow::anyhow!("Config: 'worker_secret' must be at least 16 characters"));
        }

        if self.hmac_secret.expose_secret().len() < 32 {
            return Err(anyhow::anyhow!("Config: 'hmac_secret' must be at least 32 characters"));
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
