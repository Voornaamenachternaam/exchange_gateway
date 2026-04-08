// src/caldav.rs
use crate::config::Config;
use anyhow::Result;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{RetryTransientMiddleware, policies::ExponentialBackoff};
use sha2::{Digest, Sha256};
use std::time::Duration;
use uuid::Uuid;

pub struct CaldavClient {
    base: String,
    client: ClientWithMiddleware,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
        let client = ClientBuilder::new(
            reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()?,
        )
        .with(RetryTransientMiddleware::new_with_policy(retry_policy))
        .build();
        Ok(Self {
            base: cfg.caldav_base.clone(),
            client,
        })
    }

    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let home_url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:resourcetype/>
    <D:displayname/>
  </D:prop>
</D:propfind>"#;

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home_url)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(propfind_body)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body = match r.text().await {
                    Ok(b) => b,
                    Err(e) => {
                        tracing::warn!(
                            "caldav: failed to read PROPFIND response body for {}: {} — \
                             falling back to default calendar path",
                            home_url,
                            e
                        );
                        String::new()
                    }
                };
                let hrefs = parse_calendar_collection_hrefs(&body, &home_url);
                if !hrefs.is_empty() {
                    tracing::debug!(
                        "caldav: discovered {} calendar collection(s) for {}",
                        hrefs.len(),
                        username
                    );
                    return Ok(hrefs);
                }
                tracing::warn!(
                    "caldav: PROPFIND Depth:1 on {} returned no calendar collections; \
                     falling back to well-known default path",
                    home_url
                );
            }
            Ok(r) => {
                tracing::warn!(
                    "caldav: PROPFIND on {} returned HTTP {} — falling back to default path",
                    home_url,
                    r.status()
                );
            }
            Err(e) => {
                tracing::warn!(
                    "caldav: PROPFIND on {} failed: {} — falling back to default path",
                    home_url,
                    e
                );
            }
        }

        let default_url = format!(
            "{}/cal/{}/default/",
            self.base.trim_end_matches('/'),
            username
        );
        tracing::debug!(
            "caldav: using fallback default calendar URL: {}",
            default_url
        );
        Ok(vec![default_url])
    }

    pub async fn query_events(
        &self,
        collection_href: &str,
        start: &str,
        end: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
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
</C:calendar-query>"#,
            start = start,
            end = end
        );
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"REPORT")?, collection_href)
            .basic_auth(username, Some(password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(report)
            .send()
            .await?;
        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Err(anyhow::anyhow!("failed to query events: {}", resp.status()));
        }
        Ok(resp.text().await?)
    }

    pub async fn get_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
    ) -> Result<(String, Option<String>)> {
        let url = self.absolute_url(resource_href)?;
        let resp = self
            .client
            .get(url)
            .basic_auth(username, Some(password))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("failed to fetch event: {}", resp.status()));
        }
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_string());
        Ok((resp.text().await?, etag))
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
        let target = self.resolve_resource_url(collection_href, resource_href)?;
        let mut req = self
            .client
            .put(&target)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "text/calendar; charset=utf-8")
            .body(ics.to_string());

        if let Some(etag) = if_match {
            req = req.header(IF_MATCH, etag);
        } else if resource_href.is_none() {
            req = req.header(IF_NONE_MATCH, "*");
        }

        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("failed to write event: {}", resp.status()));
        }
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_else(|| self.synthetic_etag(ics));
        Ok((self.relative_href(&target), etag))
    }

    pub async fn delete_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
        if_match: Option<&str>,
    ) -> Result<()> {
        let url = self.absolute_url(resource_href)?;
        let mut req = self.client.delete(url).basic_auth(username, Some(password));
        if let Some(etag) = if_match {
            req = req.header(IF_MATCH, etag);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            return Err(anyhow::anyhow!("failed to delete event: {}", resp.status()));
        }
        Ok(())
    }

    fn absolute_url(&self, href: &str) -> Result<String> {
        if href.starts_with("http://") || href.starts_with("https://") {
            return Ok(href.to_string());
        }
        let base = reqwest::Url::parse(&self.base)?;
        Ok(base.join(href)?.to_string())
    }

    fn resolve_resource_url(
        &self,
        collection_href: &str,
        resource_href: Option<&str>,
    ) -> Result<String> {
        if let Some(resource_href) = resource_href {
            return self.absolute_url(resource_href);
        }
        let collection = self.absolute_url(collection_href)?;
        let base = reqwest::Url::parse(&collection)?;
        Ok(base.join(&format!("{}.ics", Uuid::new_v4()))?.to_string())
    }

    fn relative_href(&self, href: &str) -> String {
        reqwest::Url::parse(href)
            .ok()
            .map(|u| {
                let mut out = u.path().to_string();
                if let Some(q) = u.query() {
                    out.push('?');
                    out.push_str(q);
                }
                out
            })
            .unwrap_or_else(|| href.to_string())
    }

    fn synthetic_etag(&self, ics: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(ics.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

use serde::Deserialize;

#[derive(Deserialize, Debug, Default)]
struct ResourceType {
    #[serde(rename = "calendar", default)]
    calendar: Option<()>,
}

#[derive(Deserialize, Debug)]
struct Prop {
    #[serde(rename = "resourcetype", default)]
    resourcetype: ResourceType,
}

#[derive(Deserialize, Debug)]
struct Multistatus {
    #[serde(rename = "response", default)]
    responses: Vec<DavResponse>,
}

#[derive(Deserialize, Debug)]
struct DavResponse {
    href: String,
    #[serde(rename = "propstat", default)]
    propstats: Vec<Propstat>,
}

#[derive(Deserialize, Debug)]
struct Propstat {
    prop: Prop,
}

fn parse_calendar_collection_hrefs(xml_body: &str, home_url: &str) -> Vec<String> {
    let multistatus: Multistatus = match quick_xml::de::from_str(xml_body) {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!("caldav: XML parse error: {}", e);
            return Vec::new();
        }
    };

    let home_url_parsed = match reqwest::Url::parse(home_url) {
        Ok(url) => Some(url),
        Err(e) => {
            tracing::error!("Failed to parse home URL {}: {}", home_url, e);
            None
        }
    };
    let home_path = home_url_parsed
        .as_ref()
        .map(|u| u.path().trim_end_matches('/').to_string())
        .unwrap_or_else(|| home_url.trim_end_matches('/').to_string());

    multistatus
        .responses
        .into_iter()
        .filter(|r| {
            r.propstats
                .iter()
                .any(|ps| ps.prop.resourcetype.calendar.is_some())
        })
        .map(|r| {
            home_url_parsed
                .as_ref()
                .and_then(|u| u.join(&r.href).ok())
                .map(|u| u.to_string())
                .unwrap_or(r.href)
        })
        .filter(|href| {
            let path = reqwest::Url::parse(href)
                .ok()
                .map(|u| u.path().trim_end_matches('/').to_string())
                .unwrap_or_else(|| href.trim_end_matches('/').to_string());
            path != home_path
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_calendar_collection_hrefs;

    const HOME_URL: &str = "http://172.28.0.10:8080/dav/cal/alice/";

    #[test]
    fn discovers_calendar_collection_excludes_home() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/dav/cal/alice/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/cal/alice/default/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let hrefs = parse_calendar_collection_hrefs(xml, HOME_URL);
        assert_eq!(hrefs.len(), 1, "home-set must be excluded");
        assert!(hrefs[0].contains("/default/"));
    }

    #[test]
    fn home_set_excluded_when_it_carries_calendar_resourcetype() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/dav/cal/alice/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/cal/alice/default/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let hrefs = parse_calendar_collection_hrefs(xml, HOME_URL);
        assert_eq!(
            hrefs.len(),
            1,
            "home-set must still be excluded even if marked as calendar"
        );
        assert!(hrefs[0].contains("/default/"));
    }

    #[test]
    fn returns_empty_when_no_calendar_resourcetype() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/cal/alice/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        assert!(parse_calendar_collection_hrefs(xml, HOME_URL).is_empty());
    }

    #[test]
    fn handles_multiple_calendar_collections() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:response>
    <D:href>/dav/cal/alice/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/cal/alice/default/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/cal/alice/work/</D:href>
    <D:propstat>
      <D:prop><D:resourcetype><D:collection/><C:calendar/></D:resourcetype></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let hrefs = parse_calendar_collection_hrefs(xml, HOME_URL);
        assert_eq!(hrefs.len(), 2);
    }
}
