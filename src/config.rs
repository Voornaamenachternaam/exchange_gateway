use serde::Deserialize;
use std::fs;
use url::Url;

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub bind: String,
    pub caldav_base: String,
    pub worker_url: String,
    pub worker_secret: String,
    pub hmac_secret: String,

    #[serde(default)]
    pub gateway_host: String,

    #[serde(default)]
    pub smtp_url: Option<String>,

    #[serde(default)]
    pub mail_domain: Option<String>,
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let content = fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Cannot read config file at '{}': {}", path, e))?;

        let mut cfg: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config TOML: {}", e))?;

        cfg.validate()?;

        if cfg.gateway_host.is_empty() {
            cfg.gateway_host = extract_host_from_url(&cfg.worker_url)
                .unwrap_or_else(|| "localhost".to_string());
        }

        if cfg.smtp_url.is_none() {
            if let Ok(v) = std::env::var("SMTP_URL") {
                if !v.is_empty() {
                    cfg.smtp_url = Some(v);
                }
            }
        }

        if cfg.mail_domain.is_none() {
            if let Ok(v) = std::env::var("MAIL_DOMAIN") {
                if !v.is_empty() {
                    cfg.mail_domain = Some(v);
                }
            }
        }

        Ok(cfg)
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        if self.bind.is_empty() {
            return Err(anyhow::anyhow!("Config: 'bind' address is required"));
        }
        if !self.bind.contains(':') {
            return Err(anyhow::anyhow!("Config: 'bind' must be in format 'host:port'"));
        }
        if self.caldav_base.is_empty() {
            return Err(anyhow::anyhow!("Config: 'caldav_base' URL is required"));
        }
        validate_url(&self.caldav_base, "caldav_base")?;

        if self.worker_url.is_empty() {
            return Err(anyhow::anyhow!("Config: 'worker_url' is required"));
        }
        validate_url(&self.worker_url, "worker_url")?;

        if self.worker_secret.is_empty() {
            return Err(anyhow::anyhow!("Config: 'worker_secret' is required"));
        }
        if self.worker_secret.len() < 16 {
            tracing::warn!("Config: worker_secret is shorter than 16 characters — this is insecure");
        }

        if self.hmac_secret.is_empty() {
            return Err(anyhow::anyhow!("Config: 'hmac_secret' is required"));
        }
        if self.hmac_secret.len() < 32 {
            tracing::warn!("Config: hmac_secret is shorter than 32 characters — this is insecure");
        }

        if !self.gateway_host.is_empty() && self.gateway_host.contains("://") {
            return Err(anyhow::anyhow!(
                "Config: 'gateway_host' must be a hostname only, not a URL"
            ));
        }

        if let Some(ref smtp) = self.smtp_url {
            if !smtp.is_empty() {
                validate_url(smtp, "smtp_url")
                    .or_else(|_| {
                        if smtp.starts_with("smtp://") || smtp.starts_with("smtps://") {
                            Ok(())
                        } else {
                            Err(anyhow::anyhow!("Config: 'smtp_url' must start with smtp:// or smtps://"))
                        }
                    })?;
            }
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
            if !matches!(parsed.scheme(), "http" | "https" | "smtp" | "smtps") {
                return Err(anyhow::anyhow!(
                    "Config: '{}' must use http, https, smtp, or smtps scheme",
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
    fn validates_empty_bind_fails() {
        let cfg = Config {
            bind: "".into(),
            caldav_base: "http://localhost".into(),
            worker_url: "https://worker.example.com/api".into(),
            worker_secret: "secret1234567890".into(),
            hmac_secret: "12345678901234567890123456789012".into(),
            gateway_host: "".into(),
            smtp_url: None,
            mail_domain: None,
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
            smtp_url: None,
            mail_domain: None,
        };
        assert!(cfg.validate().is_err());
    }
}
