// src/config.rs
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::env;
use std::fs;

const DEFAULT_MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_AUTH_CACHE_TTL_SECS: u64 = 300;
const DEFAULT_AUTH_CACHE_MAX_ENTRIES: usize = 10000;

const ENV_BIND: &str = "GATEWAY_BIND";
const ENV_CALDAV_BASE: &str = "GATEWAY_CALDAV_BASE";
const ENV_DATABASE_PATH: &str = "GATEWAY_DATABASE_PATH";
const ENV_HMAC_SECRET: &str = "GATEWAY_HMAC_SECRET";
const ENV_GATEWAY_HOST: &str = "GATEWAY_HOST";
const ENV_MAIL_DOMAIN: &str = "GATEWAY_MAIL_DOMAIN";
const ENV_MAX_ATTACHMENT_BYTES: &str = "GATEWAY_MAX_ATTACHMENT_BYTES";
const ENV_ROOM_BOOKING_ENABLED: &str = "GATEWAY_ROOM_BOOKING_ENABLED";
const ENV_AUTH_CACHE_TTL_SECS: &str = "GATEWAY_AUTH_CACHE_TTL_SECS";
const ENV_AUTH_CACHE_MAX_ENTRIES: &str = "GATEWAY_AUTH_CACHE_MAX_ENTRIES";

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
    #[serde(default = "default_room_booking_enabled")]
    pub room_booking_enabled: bool,
    #[serde(default = "default_auth_cache_ttl_secs")]
    pub auth_cache_ttl_secs: u64,
    #[serde(default = "default_auth_cache_max_entries")]
    pub auth_cache_max_entries: usize,
}

fn default_max_attachment_bytes() -> usize {
    DEFAULT_MAX_ATTACHMENT_BYTES
}

fn default_room_booking_enabled() -> bool {
    true
}

fn default_auth_cache_ttl_secs() -> u64 {
    DEFAULT_AUTH_CACHE_TTL_SECS
}

fn default_auth_cache_max_entries() -> usize {
    DEFAULT_AUTH_CACHE_MAX_ENTRIES
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("Config file not found at '{}', using environment variables only", path);
                String::new()
            }
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Cannot read config file at '{}': {}",
                    path,
                    e
                ));
            }
        };

        let secret = SecretString::from(content.clone());
        let mut cfg: Config = match toml::from_str::<Config>(secret.expose_secret()) {
            Ok(c) => c,
            Err(_) if content.trim().is_empty() => Config::default(),
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to parse config TOML at '{}': {}",
                    path,
                    e
                ));
            }
        };

        apply_environment_overrides(&mut cfg);

        if cfg.gateway_host.is_empty() {
            if let Some(host) = extract_host_from_caldav(&cfg.caldav_base) {
                cfg.gateway_host = host;
            } else if env::var(ENV_GATEWAY_HOST).is_ok() {
            } else {
                tracing::warn!(
                    "Config: 'gateway_host' not specified and could not be extracted from 'caldav_base'. Autodiscover responses may be incorrect."
                );
            }
        }
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
            return Err(anyhow::anyhow!("Config: 'bind' address is required (set via {} or config)", ENV_BIND));
        }
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config: 'bind' must be in format 'host:port'"
            ));
        }
        if self.mail_domain.trim().is_empty() {
            return Err(anyhow::anyhow!("Config: 'mail_domain' is required (set via {} or config)", ENV_MAIL_DOMAIN));
        }
        validate_url(&self.caldav_base, "caldav_base")?;
        if self.database_path.is_empty() {
            return Err(anyhow::anyhow!("Config: 'database_path' is required (set via {} or config)", ENV_DATABASE_PATH));
        }
        let secret_len = self.hmac_secret.expose_secret().len();
        if secret_len < 32 {
            return Err(anyhow::anyhow!(
                "Config: 'hmac_secret' must be at least 32 characters (set via {})",
                ENV_HMAC_SECRET
            ));
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

fn apply_environment_overrides(cfg: &mut Config) {
    match env::var(ENV_BIND) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_BIND);
            cfg.bind = val;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_BIND, e),
    }

    match env::var(ENV_CALDAV_BASE) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_CALDAV_BASE);
            cfg.caldav_base = val;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_CALDAV_BASE, e),
    }

    match env::var(ENV_DATABASE_PATH) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_DATABASE_PATH);
            cfg.database_path = val;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_DATABASE_PATH, e),
    }

    match env::var(ENV_HMAC_SECRET) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_HMAC_SECRET);
            cfg.hmac_secret = SecretString::from(val);
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_HMAC_SECRET, e),
    }

    match env::var(ENV_GATEWAY_HOST) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_GATEWAY_HOST);
            cfg.gateway_host = val;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_GATEWAY_HOST, e),
    }

    match env::var(ENV_MAIL_DOMAIN) {
        Ok(val) if !val.is_empty() => {
            tracing::debug!("Applying {} from environment", ENV_MAIL_DOMAIN);
            cfg.mail_domain = val;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_MAIL_DOMAIN, e),
    }

    match env::var(ENV_MAX_ATTACHMENT_BYTES) {
        Ok(val) => match val.parse::<usize>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_MAX_ATTACHMENT_BYTES);
                cfg.max_attachment_bytes = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_MAX_ATTACHMENT_BYTES,
                    val
                );
            }
        },
        Err(e) => tracing::debug!("{} not set: {}", ENV_MAX_ATTACHMENT_BYTES, e),
    }

    match env::var(ENV_ROOM_BOOKING_ENABLED) {
        Ok(val) if !val.is_empty() => {
            let lower = val.to_lowercase();
            tracing::debug!("Applying {} from environment", ENV_ROOM_BOOKING_ENABLED);
            cfg.room_booking_enabled = matches!(
                lower.as_str(),
                "1" | "true" | "yes" | "on" | "enabled"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::debug!("{} not set: {}", ENV_ROOM_BOOKING_ENABLED, e),
    }

    match env::var(ENV_AUTH_CACHE_TTL_SECS) {
        Ok(val) => match val.parse::<u64>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_AUTH_CACHE_TTL_SECS);
                cfg.auth_cache_ttl_secs = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_AUTH_CACHE_TTL_SECS,
                    val
                );
            }
        },
        Err(e) => tracing::debug!("{} not set: {}", ENV_AUTH_CACHE_TTL_SECS, e),
    }

    match env::var(ENV_AUTH_CACHE_MAX_ENTRIES) {
        Ok(val) => match val.parse::<usize>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_AUTH_CACHE_MAX_ENTRIES);
                cfg.auth_cache_max_entries = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_AUTH_CACHE_MAX_ENTRIES,
                    val
                );
            }
        },
        Err(e) => tracing::debug!("{} not set: {}", ENV_AUTH_CACHE_MAX_ENTRIES, e),
    }
}

fn extract_host_from_caldav(url_str: &str) -> Option<String> {
    let parsed = url::Url::parse(url_str).ok()?;
    let host = parsed.host_str()?;
    match parsed.port() {
        Some(port) => Some(format!("{host}:{port}")),
        None => Some(host.to_string()),
    }
}

fn validate_url(url: &str, field_name: &str) -> anyhow::Result<()> {
    match url::Url::parse(url) {
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

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway_host: String::new(),
            mail_domain: String::new(),
            bind: String::from("[::]:8134"),
            caldav_base: String::new(),
            database_path: String::from("/var/lib/exchange-gateway/gateway.db"),
            hmac_secret: SecretString::from(String::new()),
            max_attachment_bytes: DEFAULT_MAX_ATTACHMENT_BYTES,
            room_booking_enabled: true,
            auth_cache_ttl_secs: DEFAULT_AUTH_CACHE_TTL_SECS,
            auth_cache_max_entries: DEFAULT_AUTH_CACHE_MAX_ENTRIES,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.bind, "[::]:8134");
        assert_eq!(config.database_path, "/var/lib/exchange-gateway/gateway.db");
        assert!(config.room_booking_enabled);
    }

    #[test]
    fn test_bool_env_parsing() {
        let test_cases = vec![
            ("1", true),
            ("true", true),
            ("yes", true),
            ("on", true),
            ("enabled", true),
            ("0", false),
            ("false", false),
            ("no", false),
            ("off", false),
            ("disabled", false),
            ("anything", false),
        ];

        for (input, expected) in test_cases {
            let lower = input.to_lowercase();
            let result = matches!(
                lower.as_str(),
                "1" | "true" | "yes" | "on" | "enabled"
            );
            assert_eq!(
                result, expected,
                "Input '{}' should parse to {}",
                input, expected
            );
        }
    }

    #[test]
    fn test_hostname_validation() {
        let test_cases = vec![
            ("calendar.example.com", true),
            ("mail.example.com:8443", true),
            ("192.168.1.1", true),
            ("[::1]", true),
            ("localhost", true),
            ("https://example.com", false),
            ("http://example.com", false),
            ("example.com/path", false),
        ];

        for (host, should_pass) in test_cases {
            let is_valid = !host.contains("://") && !host.contains('/');
            assert_eq!(
                is_valid, should_pass,
                "Host '{}' validation should be {}",
                host, should_pass
            );
        }
    }
}
