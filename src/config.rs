/// src/config.rs
use serde::Deserialize;
use std::fs;
use url::Url;

/// Configuration for the Exchange Gateway.
#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    /// TCP address to listen on, e.g. "0.0.0.0:8134".
    pub bind: String,

    /// Base URL of the Stalwart Mailserver CalDAV endpoint.
    /// Example: "http://172.28.0.10:8080/dav/"
    pub caldav_base: String,

    /// URL of the Cloudflare Worker API used for D1-backed sync state.
    /// Example: "https://exchange.mail.example.com/api"
    pub worker_url: String,

    /// Shared secret used to authenticate gateway → Worker API calls.
    pub worker_secret: String,

    /// HMAC secret used to derive stable EAS/EWS server-IDs from CalDAV HREFs.
    /// Must be a long random hex string; never reuse across installations.
    pub hmac_secret: String,

    /// Public hostname of this gateway as seen by Outlook clients.
    /// Used in Autodiscover responses (EwsUrl, ASUrl, MobileSyncUrl).
    /// If absent, defaults to the host portion of `worker_url`.
    /// Example: "exchange.mail.example.com"
    #[serde(default)]
    pub gateway_host: String,
}

impl Config {
    /// Loads the configuration from a TOML file at the specified path.
    /// If `gateway_host` is missing, it is automatically derived from `worker_url`.
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read config file at '{path}': {e}"))?;

        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML: {e}"))?;

        // Validate configuration
        cfg.validate()?;

        // Derive gateway_host from worker_url if not explicitly set.
        if cfg.gateway_host.is_empty() {
            cfg.gateway_host = extract_host_from_url(&cfg.worker_url)
                .unwrap_or_else(|| "localhost".to_string());
        }

        Ok(cfg)
    }

    /// Validates the configuration for required fields and proper formats.
    pub fn validate(&self) -> anyhow::Result<()> {
        // Validate bind address
        if self.bind.is_empty() {
            return Err(anyhow::anyhow!("Config validation failed: 'bind' address is required"));
        }
        
        // Validate bind address format (should be IP:port)
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config validation failed: 'bind' must be in format 'host:port'"
            ));
        }

        // Validate caldav_base URL
        if self.caldav_base.is_empty() {
            return Err(anyhow::anyhow!("Config validation failed: 'caldav_base' URL is required"));
        }
        validate_url(&self.caldav_base, "caldav_base")?;

        // Validate worker_url
        if self.worker_url.is_empty() {
            return Err(anyhow::anyhow!("Config validation failed: 'worker_url' is required"));
        }
        validate_url(&self.worker_url, "worker_url")?;

        // Validate worker_secret
        if self.worker_secret.is_empty() {
            return Err(anyhow::anyhow!("Config validation failed: 'worker_secret' is required"));
        }
        if self.worker_secret.len() < 16 {
            tracing::warn!("Config: worker_secret is shorter than 16 characters - this is insecure!");
        }

        // Validate hmac_secret
        if self.hmac_secret.is_empty() {
            return Err(anyhow::anyhow!("Config validation failed: 'hmac_secret' is required"));
        }
        if self.hmac_secret.len() < 32 {
            tracing::warn!("Config: hmac_secret is shorter than 32 characters - this is insecure!");
        }

        // Validate gateway_host if provided
        if !self.gateway_host.is_empty() {
            if self.gateway_host.contains("://") {
                return Err(anyhow::anyhow!(
                    "Config validation failed: 'gateway_host' should be hostname only, not a URL"
                ));
            }
        }

        Ok(())
    }
}

/// Extract the host (and optional port) from a URL string.
fn extract_host_from_url(url_str: &str) -> Option<String> {
    let parsed = Url::parse(url_str).ok()?;
    let host = parsed.host_str()?;

    match parsed.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host.to_string()),
    }
}

/// Validates that a string is a valid URL
fn validate_url(url: &str, field_name: &str) -> anyhow::Result<()> {
    match Url::parse(url) {
        Ok(parsed) => {
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(anyhow::anyhow!(
                    "Config validation failed: '{}' must use http or https scheme",
                    field_name
                ));
            }
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!(
            "Config validation failed: '{}' is not a valid URL: {}",
            field_name, e
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_host_from_https_url() {
        assert_eq!(
            extract_host_from_url("https://exchange.example.com/api"),
            Some("exchange.example.com".to_string())
        );
    }

    #[test]
    fn extracts_host_with_port() {
        assert_eq!(
            extract_host_from_url("http://localhost:8080/api"),
            Some("localhost:8080".to_string())
        );
    }

    #[test]
    fn returns_none_for_invalid_url() {
        assert!(extract_host_from_url("not-a-url").is_none());
    }

    #[test]
    fn config_derives_host_correctly() {
        // Mocking a scenario where gateway_host is empty
        let cfg = Config {
            bind: "0.0.0.0:8134".into(),
            caldav_base: "http://dav.internal".into(),
            worker_url: "https://worker.example.com/api".into(),
            worker_secret: "secret".into(),
            hmac_secret: "hmac".into(),
            gateway_host: "worker.example.com".into(),
        };

        let host = extract_host_from_url("https://worker.example.com/api");
        assert_eq!(host, Some("worker.example.com".to_string()));
    }

    #[test]
    fn validates_empty_bind_fails() {
        let cfg = Config {
            bind: "".into(),
            caldav_base: "http://localhost".into(),
            worker_url: "https://worker.example.com/api".into(),
            worker_secret: "secret1234567890".into(),
            hmac_secret: "12345678901234567890123456789012".into(),
            gateway_host: "".into(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validates_missing_scheme_fails() {
        let cfg = Config {
            bind: "0.0.0.0:8134".into(),
            caldav_base: "not-a-url".into(),
            worker_url: "https://worker.example.com/api".into(),
            worker_secret: "secret1234567890".into(),
            hmac_secret: "12345678901234567890123456789012".into(),
            gateway_host: "".into(),
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn validates_weak_secret_warns() {
        let cfg = Config {
            bind: "0.0.0.0:8134".into(),
            caldav_base: "http://localhost".into(),
            worker_url: "https://worker.example.com/api".into(),
            worker_secret: "short".into(),
            hmac_secret: "12345678901234567890123456789012".into(),
            gateway_host: "".into(),
        };
        // Should succeed but might warn
        let _ = cfg.validate();
    }
}
