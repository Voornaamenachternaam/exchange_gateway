use std::env;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub jmap_url: String,
    pub db_api_url: String,
    pub db_auth_token: String,
    pub timezone: String,
    pub smtp_url: String,
    pub mail_domain: String,
}

impl AppConfig {
    pub fn from_env() -> Result<Self, String> {
        Ok(Self {
            jmap_url: env::var("JMAP_URL").map_err(|_| "JMAP_URL missing")?,
            db_api_url: env::var("CF_D1_API_URL").map_err(|_| "CF_D1_API_URL missing")?,
            db_auth_token: env::var("GATEWAY_SECRET").map_err(|_| "GATEWAY_SECRET missing")?,
            timezone: env::var("GATEWAY_TZ").map_err(|_| "GATEWAY_TZ missing")?,
            smtp_url: env::var("SMTP_URL").map_err(|_| "SMTP_URL missing")?,
            mail_domain: env::var("MAIL_DOMAIN")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    env::var("GATEWAY_HOST")
                        .ok()
                        .map(|v| v.trim().to_string())
                        .filter(|v| !v.is_empty())
                })
                .ok_or_else(|| "MAIL_DOMAIN or GATEWAY_HOST must be set and non-empty".to_string())?,
        })
    }
}
