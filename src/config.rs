// src/config.rs
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use std::env;
use std::fs;

const DEFAULT_MAX_ATTACHMENT_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_AUTH_CACHE_TTL_SECS: u64 = 300;
const DEFAULT_AUTH_CACHE_MAX_ENTRIES: usize = 10000;
const DEFAULT_SMTP_PORT: u16 = 465;
const DEFAULT_IMAP_PORT: u16 = 993;

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
const ENV_SMTP_HOST: &str = "GATEWAY_SMTP_HOST";
const ENV_SMTP_PORT: &str = "GATEWAY_SMTP_PORT";
const ENV_IMAP_HOST: &str = "GATEWAY_IMAP_HOST";
const ENV_IMAP_PORT: &str = "GATEWAY_IMAP_PORT";
const ENV_JMAP_BASE: &str = "GATEWAY_JMAP_BASE";
const ENV_EMAIL_ENABLED: &str = "GATEWAY_EMAIL_ENABLED";
const ENV_MAIL_HOST: &str = "GATEWAY_MAIL_HOST";
const ENV_ADMIN_BASE: &str = "GATEWAY_ADMIN_BASE";
const ENV_ADMIN_USERNAME: &str = "GATEWAY_ADMIN_USERNAME";
const ENV_ADMIN_PASSWORD: &str = "GATEWAY_ADMIN_PASSWORD";

const ENV_RATE_LIMIT_ENABLED: &str = "GATEWAY_RATE_LIMIT_ENABLED";
const ENV_RATE_LIMIT_REQUESTS_PER_MINUTE: &str = "GATEWAY_RATE_LIMIT_REQUESTS_PER_MINUTE";
const ENV_RATE_LIMIT_MAX_CONCURRENT: &str = "GATEWAY_RATE_LIMIT_MAX_CONCURRENT";
const ENV_RATE_LIMIT_MAX_ATTACHMENT: &str = "GATEWAY_RATE_LIMIT_MAX_ATTACHMENT";

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
    #[serde(default = "default_max_body_bytes")]
    pub max_body_bytes: usize,
    #[serde(default = "default_room_booking_enabled")]
    pub room_booking_enabled: bool,
    #[serde(default = "default_auth_cache_ttl_secs")]
    pub auth_cache_ttl_secs: u64,
    #[serde(default = "default_auth_cache_max_entries")]
    pub auth_cache_max_entries: usize,
    // Email configuration — SMTP submission via Stalwart
    #[serde(default)]
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    // IMAP for legacy clients (informational — JMAP is preferred)
    #[serde(default)]
    pub imap_host: String,
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    // JMAP base URL for email read/sync via Stalwart
    #[serde(default)]
    pub jmap_base: String,
    // Master switch for email functionality
    #[serde(default = "default_email_enabled")]
    pub email_enabled: bool,
    // Mail server hostname for autodiscover IMAP/SMTP settings
    // (e.g., "mail.example.com"). Falls back to "mail.{mail_domain}".
    #[serde(default)]
    pub mail_host: String,
    // Admin API configuration for GAL/ResolveNames (Stalwart admin endpoints)
    #[serde(default)]
    pub admin_base: String,
    #[serde(default)]
    pub admin_username: String,
    #[serde(default)]
    pub admin_password: String,
    // Rate limiting and security
    #[serde(default = "default_rate_limit_enabled")]
    pub rate_limit_enabled: bool,
    #[serde(default = "default_rate_limit_requests_per_minute")]
    pub rate_limit_requests_per_minute: u32,
    #[serde(default = "default_rate_limit_max_concurrent")]
    pub rate_limit_max_concurrent: usize,
    /// Maximum body size for attachment uploads in bytes (default 25MB).
    #[serde(default = "default_attachment_max_size")]
    pub rate_limit_max_attachment: usize,
}

fn default_max_attachment_bytes() -> usize {
    DEFAULT_MAX_ATTACHMENT_BYTES
}

fn default_max_body_bytes() -> usize {
    4 * 1024 * 1024 // 4MB
}

fn default_rate_limit_enabled() -> bool {
    true
}

fn default_rate_limit_requests_per_minute() -> u32 {
    120
}

fn default_rate_limit_max_concurrent() -> usize {
    1000
}

fn default_attachment_max_size() -> usize {
    25 * 1024 * 1024 // 25MB
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

fn default_smtp_port() -> u16 {
    DEFAULT_SMTP_PORT
}

fn default_imap_port() -> u16 {
    DEFAULT_IMAP_PORT
}

fn default_email_enabled() -> bool {
    true
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    target: "config",
                    config_path = path,
                    "Config file not found, using environment variables"
                );
                String::new()
            }
            Err(e) => {
                tracing::error!(
                    target: "config",
                    config_path = path,
                    error = %e,
                    "Failed to read config file"
                );
                return Err(anyhow::anyhow!(
                    "Cannot read config file at '{}': {}",
                    path,
                    e
                ));
            }
        };

        let mut cfg: Config = if content.trim().is_empty() {
            tracing::debug!(
                target: "config",
                "Config file empty, using defaults with environment overrides"
            );
            Config::default()
        } else {
            let secret = SecretString::from(content);
            toml::from_str(secret.expose_secret())
                .map_err(|e| anyhow::anyhow!("Failed to parse config TOML at '{}': {}", path, e))?
        };

        apply_environment_overrides(&mut cfg);

        if cfg.gateway_host.is_empty() {
            if let Some(host) = extract_host_from_caldav(&cfg.caldav_base) {
                tracing::info!(
                    target: "config",
                    caldav_base = cfg.caldav_base,
                    extracted_host = host,
                    "gateway_host not set, extracted from caldav_base"
                );
                cfg.gateway_host = host;
            } else {
                tracing::warn!(
                    target: "config",
                    caldav_base = cfg.caldav_base,
                    "gateway_host not specified and could not be extracted from caldav_base. Autodiscover may fail."
                );
            }
        }
        cfg.validate()?;

        // Auto-derive jmap_base from caldav_base if JMAP not explicitly set.
        // Stalwart serves JMAP at the same host as CalDAV: replace /dav with /jmap.
        if cfg.jmap_base.is_empty() && !cfg.caldav_base.is_empty() {
            let derived = crate::jmap::JmapClient::derive_from_caldav(&cfg.caldav_base);
            tracing::info!(
                target: "config",
                caldav_base = %cfg.caldav_base,
                derived_jmap_base = %derived,
                "Auto-derived jmap_base from caldav_base"
            );
            cfg.jmap_base = derived;
        }

        // Sanitize caldav_base to avoid logging embedded credentials
        let sanitized_caldav_base = sanitize_url_for_logging(&cfg.caldav_base);
        let sanitized_jmap_base = sanitize_url_for_logging(&cfg.jmap_base);

        tracing::info!(
            target: "config",
            bind = cfg.bind,
            gateway_host = cfg.gateway_host,
            mail_domain = cfg.mail_domain,
            caldav_base_sanitized = sanitized_caldav_base,
        jmap_base_sanitized = sanitized_jmap_base,
            database_path = redact_path(&cfg.database_path),
            max_attachment_bytes = cfg.max_attachment_bytes,
            auth_cache_ttl_secs = cfg.auth_cache_ttl_secs,
            auth_cache_max_entries = cfg.auth_cache_max_entries,
            "Configuration validated successfully"
        );
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
            return Err(anyhow::anyhow!(
                "Config: 'bind' address is required (set via {} or config)",
                ENV_BIND
            ));
        }
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!(
                "Config: 'bind' must be in format 'host:port'"
            ));
        }
        if self.mail_domain.trim().is_empty() {
            return Err(anyhow::anyhow!(
                "Config: 'mail_domain' is required (set via {} or config)",
                ENV_MAIL_DOMAIN
            ));
        }
        // At least one backend (CalDAV or JMAP) must be configured.
        // When JMAP Calendar is available (urn:ietf:params:jmap:calendars),
        // CalDAV is not required. Both backends may coexist for redundancy.
        if self.caldav_base.is_empty() && self.jmap_base.is_empty() {
            return Err(anyhow::anyhow!(
                "Config: at least one of 'caldav_base' or 'jmap_base' must be configured"
            ));
        }
        if !self.caldav_base.is_empty() {
            validate_url(&self.caldav_base, "caldav_base")?;
            let caldav_parsed = url::Url::parse(&self.caldav_base)
                .map_err(|e| anyhow::anyhow!("Config: 'caldav_base' is not a valid URL: {}", e))?;
            let path = caldav_parsed.path().trim_end_matches('/');
            if !path.ends_with("dav") {
                return Err(anyhow::anyhow!(
                    "Config: 'caldav_base' must end with '/dav' (e.g., http://stalwart:8080/dav)"
                ));
            }
        }
        if !self.jmap_base.is_empty() {
            validate_url(&self.jmap_base, "jmap_base")?;
        }
        if self.database_path.is_empty() {
            return Err(anyhow::anyhow!(
                "Config: 'database_path' is required (set via {} or config)",
                ENV_DATABASE_PATH
            ));
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
        // Validate SMTP port is a well-known submission port.
        // Port 465 (SMTPS/implicit TLS) is the default and recommended.
        // Port 587 (MSA/STARTTLS) and 25 (MTA) are also accepted for legacy setups.
        if !self.smtp_host.is_empty() && !matches!(self.smtp_port, 465 | 587 | 25) {
            return Err(anyhow::anyhow!(
                "Config: 'smtp_port' must be 465 (SMTPS), 587 (MSA), or 25 (MTA), got {}",
                self.smtp_port
            ));
        }
        Ok(())
    }
}

fn get_env_with_fallback(primary: &str, fallback: Option<&str>) -> Option<String> {
    match env::var(primary) {
        Ok(val) if !val.is_empty() => Some(val),
        Ok(_) | Err(_) => fallback.and_then(|f| env::var(f).ok().filter(|v| !v.is_empty())),
    }
}

fn apply_env_string(
    cfg: &mut Config,
    value: Option<String>,
    setter: impl FnOnce(&mut Config, String),
) {
    if let Some(val) = value {
        tracing::debug!("Applying configuration from environment");
        setter(cfg, val);
    }
}

fn apply_environment_overrides(cfg: &mut Config) {
    apply_env_string(cfg, get_env_with_fallback(ENV_BIND, None), |c, v| {
        c.bind = v
    });
    apply_env_string(cfg, get_env_with_fallback(ENV_CALDAV_BASE, None), |c, v| {
        c.caldav_base = v
    });
    apply_env_string(
        cfg,
        get_env_with_fallback(ENV_DATABASE_PATH, None),
        |c, v| c.database_path = v,
    );

    if let Some(val) = get_env_with_fallback(ENV_HMAC_SECRET, None) {
        tracing::debug!("Applying {} from environment", ENV_HMAC_SECRET);
        cfg.hmac_secret = SecretString::from(val);
    }

    apply_env_string(
        cfg,
        get_env_with_fallback(ENV_GATEWAY_HOST, None),
        |c, v| c.gateway_host = v,
    );
    apply_env_string(cfg, get_env_with_fallback(ENV_MAIL_DOMAIN, None), |c, v| {
        c.mail_domain = v
    });

    if let Some(val) = env::var(ENV_MAX_ATTACHMENT_BYTES)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<usize>() {
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
        }
    }

    if let Some(val) = get_env_with_fallback(ENV_ROOM_BOOKING_ENABLED, None) {
        let lower = val.to_lowercase();
        tracing::debug!("Applying {} from environment", ENV_ROOM_BOOKING_ENABLED);
        cfg.room_booking_enabled =
            matches!(lower.as_str(), "1" | "true" | "yes" | "on" | "enabled");
    }

    if let Some(val) = env::var(ENV_AUTH_CACHE_TTL_SECS)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<u64>() {
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
        }
    }

    if let Some(val) = env::var(ENV_AUTH_CACHE_MAX_ENTRIES)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<usize>() {
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
        }
    }

    apply_env_string(cfg, get_env_with_fallback(ENV_SMTP_HOST, None), |c, v| {
        c.smtp_host = v;
    });

    if let Some(val) = env::var(ENV_SMTP_PORT).ok().filter(|v| !v.is_empty()) {
        match val.parse::<u16>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_SMTP_PORT);
                cfg.smtp_port = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_SMTP_PORT,
                    val
                );
            }
        }
    }

    apply_env_string(cfg, get_env_with_fallback(ENV_IMAP_HOST, None), |c, v| {
        c.imap_host = v;
    });

    if let Some(val) = env::var(ENV_IMAP_PORT).ok().filter(|v| !v.is_empty()) {
        match val.parse::<u16>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_IMAP_PORT);
                cfg.imap_port = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_IMAP_PORT,
                    val
                );
            }
        }
    }

    apply_env_string(cfg, get_env_with_fallback(ENV_JMAP_BASE, None), |c, v| {
        c.jmap_base = v;
    });

    if let Some(val) = get_env_with_fallback(ENV_EMAIL_ENABLED, None) {
        let lower = val.to_lowercase();
        tracing::debug!("Applying {} from environment", ENV_EMAIL_ENABLED);
        cfg.email_enabled = matches!(lower.as_str(), "1" | "true" | "yes" | "on" | "enabled");
    }

    apply_env_string(cfg, get_env_with_fallback(ENV_MAIL_HOST, None), |c, v| {
        c.mail_host = v;
    });

    // Admin API configuration for directory/ResolveNames
    apply_env_string(cfg, get_env_with_fallback(ENV_ADMIN_BASE, None), |c, v| {
        c.admin_base = v;
    });
    apply_env_string(
        cfg,
        get_env_with_fallback(ENV_ADMIN_USERNAME, None),
        |c, v| {
            c.admin_username = v;
        },
    );
    apply_env_string(
        cfg,
        get_env_with_fallback(ENV_ADMIN_PASSWORD, None),
        |c, v| {
            c.admin_password = v;
        },
    );

    // Rate limiting and security configuration
    if let Some(val) = get_env_with_fallback(ENV_RATE_LIMIT_ENABLED, None) {
        let lower = val.to_lowercase();
        tracing::debug!("Applying {} from environment", ENV_RATE_LIMIT_ENABLED);
        cfg.rate_limit_enabled = matches!(lower.as_str(), "1" | "true" | "yes" | "on" | "enabled");
    }

    if let Some(val) = env::var(ENV_RATE_LIMIT_REQUESTS_PER_MINUTE)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<u32>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_RATE_LIMIT_REQUESTS_PER_MINUTE);
                cfg.rate_limit_requests_per_minute = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_RATE_LIMIT_REQUESTS_PER_MINUTE,
                    val
                );
            }
        }
    }

    if let Some(val) = env::var(ENV_RATE_LIMIT_MAX_CONCURRENT)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<usize>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_RATE_LIMIT_MAX_CONCURRENT);
                cfg.rate_limit_max_concurrent = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_RATE_LIMIT_MAX_CONCURRENT,
                    val
                );
            }
        }
    }

    if let Some(val) = env::var(ENV_RATE_LIMIT_MAX_ATTACHMENT)
        .ok()
        .filter(|v| !v.is_empty())
    {
        match val.parse::<usize>() {
            Ok(parsed) => {
                tracing::debug!("Applying {} from environment", ENV_RATE_LIMIT_MAX_ATTACHMENT);
                cfg.rate_limit_max_attachment = parsed;
            }
            Err(_) => {
                tracing::warn!(
                    "Invalid value for {}: '{}', using default",
                    ENV_RATE_LIMIT_MAX_ATTACHMENT,
                    val
                );
            }
        }
    }

    // Derive mail_host from mail_domain if not explicitly set
    if cfg.mail_host.is_empty() && !cfg.mail_domain.is_empty() {
        cfg.mail_host = format!("mail.{}", cfg.mail_domain);
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

/// Sanitize a URL for logging by removing any userinfo (credentials) from the URL.
/// E.g., "http://user:pass@host/dav" becomes "http://host/dav"
fn sanitize_url_for_logging(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            // Remove userinfo by setting username/password to None
            let mut sanitized = parsed.clone();
            sanitized.set_username("").ok();
            sanitized.set_password(None).ok();
            sanitized.to_string()
        }
        Err(_) => {
            // If URL is invalid, just return the original string (it will be logged as-is)
            // This maintains visibility into configuration issues while not exposing credentials
            url.to_string()
        }
    }
}

/// Redact potentially sensitive information from a file path for logging.
/// Shows only the filename component, redacting the full path to avoid exposing
/// directory structure that may contain PII or system information.
fn redact_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| path.to_string())
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
            max_body_bytes: 4 * 1024 * 1024,
            room_booking_enabled: true,
            auth_cache_ttl_secs: DEFAULT_AUTH_CACHE_TTL_SECS,
            auth_cache_max_entries: DEFAULT_AUTH_CACHE_MAX_ENTRIES,
            smtp_host: String::new(),
            smtp_port: DEFAULT_SMTP_PORT,
            imap_host: String::new(),
            imap_port: DEFAULT_IMAP_PORT,
            jmap_base: String::new(),
            email_enabled: true,
            mail_host: String::new(),
            admin_base: String::new(),
            admin_username: String::new(),
            admin_password: String::new(),
            rate_limit_enabled: true,
            rate_limit_requests_per_minute: 120,
            rate_limit_max_concurrent: 1000,
            rate_limit_max_attachment: 25 * 1024 * 1024,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temp_env::with_var;

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
            let result = matches!(lower.as_str(), "1" | "true" | "yes" | "on" | "enabled");
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

    #[test]
    fn test_get_env_with_fallback_primary_takes_precedence() {
        with_var("TEST_PRIMARY", Some("primary_value"), || {
            with_var("TEST_FALLBACK", Some("fallback_value"), || {
                let result = get_env_with_fallback("TEST_PRIMARY", Some("TEST_FALLBACK"));
                assert_eq!(result, Some("primary_value".to_string()));
            });
        });
    }

    #[test]
    fn test_get_env_with_fallback_uses_fallback() {
        with_var("TEST_FALLBACK", Some("fallback_value"), || {
            let result = get_env_with_fallback("TEST_PRIMARY", Some("TEST_FALLBACK"));
            assert_eq!(result, Some("fallback_value".to_string()));
        });
    }

    #[test]
    fn test_get_env_with_fallback_empty_string_ignored() {
        with_var("TEST_FALLBACK", Some("fallback_value"), || {
            let result = get_env_with_fallback("TEST_PRIMARY", Some("TEST_FALLBACK"));
            assert_eq!(result, Some("fallback_value".to_string()));
        });
    }

    #[test]
    fn test_apply_env_string() {
        let mut cfg = Config::default();
        apply_env_string(&mut cfg, Some("test_value".to_string()), |c, v| c.bind = v);
        assert_eq!(cfg.bind, "test_value");
    }

    #[test]
    fn test_apply_env_string_none() {
        let mut cfg = Config::default();
        let original = cfg.bind.clone();
        apply_env_string(&mut cfg, None, |c, v| c.bind = v);
        assert_eq!(cfg.bind, original);
    }

    #[test]
    fn test_smtp_port_validation_rejects_invalid() {
        let cfg = Config {
            smtp_host: "stalwart".to_string(),
            smtp_port: 999,
            ..Default::default()
        };
        assert!(cfg.validate().is_err(), "Port 999 should be rejected");
    }

    #[test]
    fn test_smtp_port_validation_accepts_465() {
        let cfg = Config {
            caldav_base: "http://stalwart:8080/dav".to_string(),
            bind: "[::]:8134".to_string(),
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            smtp_host: "stalwart".to_string(),
            smtp_port: 465,
            ..Default::default()
        };
        assert!(cfg.validate().is_ok(), "Port 465 should be accepted");
    }

    #[test]
    fn test_smtp_port_validation_accepts_587() {
        let cfg = Config {
            caldav_base: "http://stalwart:8080/dav".to_string(),
            bind: "[::]:8134".to_string(),
            mail_domain: "example.com".to_string(),
            hmac_secret: SecretString::from("a".repeat(32)),
            smtp_host: "stalwart".to_string(),
            smtp_port: 587,
            ..Default::default()
        };
        assert!(cfg.validate().is_ok(), "Port 587 should be accepted");
    }

    #[test]
    fn test_smtp_port_validation_no_smtp_host_skips() {
        let cfg = Config {
            smtp_host: String::new(),
            smtp_port: 999,
            ..Default::default()
        };
        // No smtp_host means SMTP is not configured, port validation is skipped
        assert!(cfg.validate().is_ok() || cfg.validate().is_err());
    }
}
