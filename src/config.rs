use std::env;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub jmap_url: String,
    pub db_api_url: String,
    pub db_auth_token: String,
    pub timezone: String,
    pub smtp_url: url::Url,
    pub mail_domain: String,
    pub gateway_external_url: Option<String>,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            jmap_url: env::var("JMAP_URL").map_err(|_| "JMAP_URL missing")?,
            db_api_url: env::var("CF_D1_API_URL").map_err(|_| "CF_D1_API_URL missing")?,
            db_auth_token: env::var("GATEWAY_SECRET").map_err(|_| "GATEWAY_SECRET missing")?,
            timezone: env::var("GATEWAY_TZ").map_err(|_| "GATEWAY_TZ missing")?,
            smtp_url: {
                let smtp_url = env::var("SMTP_URL")
                    .map_err(|_| "SMTP_URL missing".to_string())?
                    .parse::<url::Url>()
                    .map_err(|e| format!("Invalid SMTP_URL: {}", e))?;
                match smtp_url.scheme() {
                    "smtp" | "smtps" | "starttls" => {}
                    _ => {
                        return Err(
                            "SMTP_URL must use smtp://, smtps://, or starttls://".to_string()
                        );
                    }
                }
                if smtp_url.host_str().is_none() {
                    return Err("SMTP_URL must include a host".to_string());
                }
                smtp_url
            },
            mail_domain: {
                let domain = env::var("MAIL_DOMAIN")
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty());
                if domain.is_none() {
                    if env::var("GATEWAY_HOST")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                        .is_some()
                    {
                        tracing::warn!(
                            "GATEWAY_HOST is set but MAIL_DOMAIN is not. \
                             GATEWAY_HOST is no longer used as a mail-domain fallback. \
                             Please set MAIL_DOMAIN to your email domain explicitly."
                        );
                    }
                }
                domain.ok_or_else(|| "MAIL_DOMAIN must be set and non-empty".to_string())?
            },
            gateway_external_url: env::var("GATEWAY_EXTERNAL_URL")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
        })
    }
}
