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
        // Convention: Stalwart calendar home at {base}/{username}/calendar/
        let url = format!("{}cal/{}", self.base.trim_end_matches('/'), username);
        let resp = self.client.get(&url).basic_auth(username, Some(password)).send().await?;
        if resp.status().is_success() {
            Ok(vec![url])
        } else {
            Err(anyhow::anyhow!("failed to discover calendars: {}", resp.status()))
        }
    }

    pub async fn query_events(&self, collection_href: &str, start: &str, end: &str, username: &str, password: &str) -> Result<String> {
        let report = format!(r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{start}" end="{end}" />
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#, start=start, end=end);

        let resp = self.client.request(reqwest::Method::from_bytes(b"REPORT")?, collection_href)
            .basic_auth(username, Some(password))
            .header("Content-Type","application/xml")
            .body(report)
            .send().await?;
        let txt = resp.text().await?;
        Ok(txt)
    }

    pub async fn get_event(&self, resource_href: &str, username: &str, password: &str) -> Result<String> {
        let resp = self.client.get(resource_href).basic_auth(username, Some(password)).send().await?;
        let txt = resp.text().await?;
        Ok(txt)
    }

    pub async fn put_event(&self, collection_href: &str, resource_name: &str, ics: &str, username: &str, password: &str) -> Result<String> {
        let url = format!("{}/{}", collection_href.trim_end_matches('/'), resource_name);
        let resp = self.client.put(&url).basic_auth(username, Some(password)).body(ics.to_string()).header("Content-Type","text/calendar; charset=utf-8").send().await?;
        let etag = resp.headers().get("ETag").map(|v| v.to_str().unwrap_or("").to_string()).unwrap_or_default();
        if resp.status().is_success() { Ok(etag) } else { Err(anyhow::anyhow!("put failed: {}", resp.status())) }
    }

    pub async fn delete_event(&self, resource_href: &str, username: &str, password: &str) -> Result<()> {
        let resp = self.client.delete(resource_href).basic_auth(username, Some(password)).send().await?;
        if resp.status().is_success() || resp.status().as_u16() == 204 { Ok(()) } else { Err(anyhow::anyhow!("delete failed: {}", resp.status())) }
    }
}
