// src/caldav.rs
use crate::config::Config;
use anyhow::Result;
use reqwest::Client;

pub struct CaldavClient {
    base: String,
    client: Client,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Self {
        let client = Client::builder().build().unwrap();
        CaldavClient { base: cfg.caldav_base.clone(), client }
    }

    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let url = format!("{}cal/{}/", self.base.trim_end_matches('/'), username);
        let resp = self.client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &url)
            .basic_auth(username, Some(password))
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<propfind xmlns="DAV:"><prop><resourcetype/></prop></propfind>"#)
            .send().await?;
        if resp.status().is_success() || resp.status().as_u16() == 207 { Ok(vec![url]) }
        else { Err(anyhow::anyhow!("failed to discover calendars: {}", resp.status())) }
    }

    pub async fn query_events(&self, collection_href: &str, start: &str, end: &str, username: &str, password: &str) -> Result<String> {
        let report = format!(
            r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><D:getetag/><C:calendar-data/></D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{start}" end="{end}" />
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#, start = start, end = end
        );
        let resp = self.client
            .request(reqwest::Method::from_bytes(b"REPORT")?, collection_href)
            .basic_auth(username, Some(password)).header("Content-Type", "application/xml").header("Depth", "1").body(report).send().await?;
        Ok(resp.text().await?)
     }
}
