use crate::config::Config;
use anyhow::{anyhow, Result};
use reqwest::Client;

pub struct CaldavClient {
    base_url: String,
    http: Client,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Self {
        Self {
            base_url: cfg.caldav_base.clone(),
            http: Client::new(),
        }
    }

    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let url = format!("{}calendars/{}/", self.base_url, username);

        let response = self
            .http
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &url)
            .basic_auth(username, Some(password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:resourcetype/>
    <D:displayname/>
    <C:supported-calendar-component-set/>
  </D:prop>
</D:propfind>"#)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("PROPFIND failed: {}", response.status()));
        }

        let body = response.text().await?;
        let calendars = parse_calendar_hrefs(&body, &url);

        Ok(calendars)
    }

    pub async fn query_events(
        &self,
        collection_href: &str,
        start: &str,
        end: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let query = format!(r#"<?xml version="1.0" encoding="utf-8"?>
<C:calendar-query xmlns:C="urn:ietf:params:xml:ns:caldav" xmlns:D="DAV:">
  <D:prop>
    <D:getetag/>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="{}" end="{}"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#,
            start, end
        );

        let response = self
            .http
            .request(reqwest::Method::from_bytes(b"REPORT")?, collection_href)
            .basic_auth(username, Some(password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(query)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("REPORT failed: {}", response.status()));
        }

        Ok(response.text().await?)
    }

    pub async fn get_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
    ) -> Result<(String, Option<String>)> {
        let response = self
            .http
            .get(resource_href)
            .basic_auth(username, Some(password))
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!("GET failed: {}", response.status()));
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let body = response.text().await?;
        Ok((body, etag))
    }

    pub async fn put_event(
        &self,
        collection_href: &str,
        resource_href: Option<&str>,
        ics: &str,
        username: &str,
        password: &str,
        if_match: Option<&str>,
    ) -> Result<(String, String)> {
        let url = resource_href
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}{}.ics", collection_href, uuid::Uuid::new_v4()));

        let mut request = self
            .http
            .put(&url)
            .basic_auth(username, Some(password))
            .header("Content-Type", "text/calendar; charset=utf-8");

        if let Some(etag) = if_match {
            request = request.header("If-Match", etag);
        }

        let response = request.body(ics.to_string()).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("PUT failed: {}", response.status()));
        }

        let etag = response
            .headers()
            .get("etag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        Ok((url, etag))
    }

    pub async fn delete_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
        if_match: Option<&str>,
    ) -> Result<()> {
        let mut request = self
            .http
            .delete(resource_href)
            .basic_auth(username, Some(password));

        if let Some(etag) = if_match {
            request = request.header("If-Match", etag);
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("DELETE failed: {}", response.status()));
        }

        Ok(())
    }
}

fn parse_calendar_hrefs(xml: &str, base_url: &str) -> Vec<String> {
    let mut hrefs = Vec::new();
    let mut in_calendar = false;
    let mut current_href = String::new();

    for line in xml.lines() {
        if line.contains("<href>") || line.contains("<D:href>") {
            if let Some(start) = line.find('>') {
                if let Some(end) = line.rfind('<') {
                    current_href = line[start + 1..end].to_string();
                    if !current_href.starts_with("http") {
                        current_href = format!("{}{}", base_url, current_href.trim_start_matches('/'));
                    }
                }
            }
        } else if line.contains("<resourcetype>") || line.contains("<D:resourcetype>") {
            in_calendar = true;
        } else if line.contains("</resourcetype>") || line.contains("</D:resourcetype>") {
            in_calendar = false;
        } else if (line.contains("<calendar/>") || line.contains("<C:calendar/>")) && in_calendar {
            if !current_href.is_empty() && !hrefs.contains(&current_href) {
                hrefs.push(current_href.clone());
            }
        }
    }

    hrefs
}
