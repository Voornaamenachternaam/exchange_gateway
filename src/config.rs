use std::env;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub jmap_url: String,
    pub db_api_url: String,
    pub db_auth_token: String,
    pub timezone: String,
    pub smtp_url: url::Url,
    pub mail_domain: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            jmap_url: env::var("JMAP_URL").map_err(|_| "JMAP_URL missing")?,
            db_api_url: env::var("CF_D1_API_URL").map_err(|_| "CF_D1_API_URL missing")?,
            db_auth_token: env::var("GATEWAY_SECRET").map_err(|_| "GATEWAY_SECRET missing")?,
            timezone: env::var("GATEWAY_TZ").map_err(|_| "GATEWAY_TZ missing")?,
            smtp_url: {
                let raw = env::var("SMTP_URL").map_err(|_| "SMTP_URL missing")?;
                let url = url::Url::parse(&raw)
                    .map_err(|_| "SMTP_URL must be a valid URL".to_string())?;
                if !matches!(url.scheme(), "smtp" | "smtps") || url.host_str().is_none() {
                    return Err("SMTP_URL must use smtp:// or smtps:// and include a host".to_string());
                }
                url
            },
            },
        })
    }
}
