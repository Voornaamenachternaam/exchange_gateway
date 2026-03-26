// src/config.rs
//
// Gap closed: Autodiscover responses require a gateway_host value so they can
// construct correct EwsUrl / ASUrl / MobileSyncUrl endpoint strings. The
// previous config only carried bind/caldav_base/worker_url/worker_secret/hmac_secret.
// A new optional `gateway_host` field is added; if absent it defaults to the
// hostname portion of `worker_url` (which is the Cloudflare-published hostname
// in all reference deployments).

use serde::Deserialize;
use std::fs;

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
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let s = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read config file at '{}': {}", path, e))?;
        let mut cfg: Config = toml::from_str(&s)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML: {}", e))?;

        // Derive gateway_host from worker_url if not explicitly set.
        if cfg.gateway_host.is_empty() {
            cfg.gateway_host = extract_host_from_url(&cfg.worker_url)
                .unwrap_or_else(|| "localhost".to_string());
        }

        Ok(cfg)
    }
}

/// Extract the host (and optional port) from a URL string.
fn extract_host_from_url(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.to_string())
}
    let url = url.trim();
    let after_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Strip any path component.
    let host_and_port = after_scheme.split('/').next()?;
    if host_and_port.is_empty() {
        return None;
    }
    Some(host_and_port.to_string())
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
}
