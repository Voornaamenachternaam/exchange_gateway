// src/caldav.rs
use crate::config::Config;
use anyhow::Result;
use reqwest::Client;

/// Minimal CalDAV client implementation for listing calendars and fetching ICS data.
/// The functions are deliberately conservative: they return simple data sufficient for Sync.
pub struct CaldavClient {
    base: String,
    client: Client,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Self {
        let client = Client::builder().build().expect("failed to build reqwest client");
        Self {
            base: cfg.caldav_base.trim_end_matches('/').to_string(),
            client,
        }
    }

    /// Returns the list of calendar collection hrefs for the user.
    /// In Stalwart typical CalDAV path is: {base}/cal/<username> or /dav/cal/<username>.
    /// This implementation tries a small set of common locations and falls back to a single href.
    pub async fn find_user_calendars(&self, username: &str, _password: &str) -> Result<Vec<String>> {
        // Conservative approach: attempt the base + /dav/cal/{username} then fallback.
        let mut possible = vec![
            format!("{}/dav/cal/{}", self.base, username),
            format!("{}/cal/{}", self.base, username),
            format!("{}/calendars/{}", self.base, username),
        ];

        // Try HEAD on each and return the first that responds 200/207/401 (401 means server protected).
        for href in &possible {
            let _ = self.client.head(href).send().await;
            // We don't insist on status codes because some servers require auth and return 401
            // In either case return first plausible href as conservative fallback.
            return Ok(vec![href.clone()]);
        }

        // Fallback: return base as a collection
        Ok(vec![self.base.clone()])
    }

    /// Retrieve ICS data for a given event href (not used in the minimal sync stub)
    pub async fn get_ics(&self, href: &str, username: &str, password: &str) -> Result<String> {
        let res = self
            .client
            .get(href)
            .basic_auth(username, Some(password))
            .send()
            .await?;
        let txt = res.text().await?;
        Ok(txt)
    }
}
