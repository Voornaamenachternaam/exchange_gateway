// src/caldav.rs
use crate::config::Config;
use anyhow::Result;
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use quick_xml::Reader;
use quick_xml::events::Event;
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct CaldavClient {
    base: String,
    client: Client,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Self {
        let client = Client::builder()
            .http1_only()
            .pool_max_idle_per_host(8)
            .build()
            .expect("reqwest client construction should be infallible for static config");
        CaldavClient {
            base: cfg.caldav_base.clone(),
            client,
        }
    }

    /// Discover the calendar collections for a user.
    ///
    /// Issues a PROPFIND Depth:1 on the CalDAV calendar home
    /// (`{caldav_base}/cal/{username}/`) to find actual calendar collection URLs
    /// (resourcetype includes `<C:calendar/>`). This is required by RFC 4791 §7.8:
    /// a `calendar-query` REPORT is only valid on a calendar *collection*, not on
    /// the calendar home-set.
    ///
    /// For Stalwart v0.15.5 the default calendar is at
    /// `/dav/cal/{username}/default/`. If PROPFIND discovery finds no collections
    /// (e.g. due to a transient error or unusual server layout), we fall back to
    /// that well-known path.
    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let home_url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);

        // PROPFIND Depth:1 to enumerate calendar collections under the home.
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
            Ok(r) if r.status().is_success() || r.status().as_u16() == 207 => {
                let body = r.text().await.unwrap_or_default();
                let hrefs = parse_calendar_collection_hrefs(&body, &self.base);
                if !hrefs.is_empty() {
                    tracing::debug!(
                        "caldav: discovered {} calendar collection(s) for {}",
                        hrefs.len(),
                        username
                    );
                    return Ok(hrefs);
                }
                // Fall through to default path
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

        // Fallback: Stalwart v0.15.5 default calendar collection path.
        let default_url = format!("{}/cal/{}/default/", self.base.trim_end_matches('/'), username);
        tracing::debug!("caldav: using fallback default calendar URL: {}", default_url);
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

/// Parse a PROPFIND Depth:1 multistatus response and return the absolute URLs
/// of all responses whose `DAV:resourcetype` includes `<C:calendar/>`.
///
/// This correctly handles both:
/// - namespace-prefixed element names: `<C:calendar/>`
/// - local-name-only element names: `<calendar/>`
fn parse_calendar_collection_hrefs(xml_body: &str, base_url: &str) -> Vec<String> {
    let mut reader = Reader::from_str(xml_body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();

    let mut current_href = String::new();
    let mut in_href = false;
    let mut is_calendar = false;
    let mut in_response = false;
    let mut results = Vec::new();

    // Pre-compute base for relative URL resolution.
    let base = reqwest::Url::parse(base_url).ok();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"response" => {
                        in_response = true;
                        current_href.clear();
                        is_calendar = false;
                    }
                    b"href" if in_response => {
                        in_href = true;
                    }
                    b"calendar" => {
                        is_calendar = true;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(ref e)) => {
                let local = e.name().local_name();
                if local.as_ref() == b"calendar" {
                    is_calendar = true;
                }
            }
            Ok(Event::Text(ref t)) if in_href => {
                if let Ok(text) = t.decode() {
                    current_href = text.trim().to_string();
                }
            }
            Ok(Event::End(ref e)) => {
                let local = e.name().local_name();
                match local.as_ref() {
                    b"href" => {
                        in_href = false;
                    }
                    b"response" => {
                        if in_response && is_calendar && !current_href.is_empty() {
                            // Build absolute URL from href (which may be relative).
                            let abs = if current_href.starts_with("http://")
                                || current_href.starts_with("https://")
                            {
                                current_href.clone()
                            } else if let Some(ref b) = base {
                                b.join(&current_href)
                                    .ok()
                                    .map(|u| u.to_string())
                                    .unwrap_or_else(|| current_href.clone())
                            } else {
                                current_href.clone()
                            };
                            // Exclude the home itself — it will appear at Depth:0
                            // in the Depth:1 response, but it is NOT a calendar
                            // collection (it is the calendar home-set).
                            // We identify it by checking it is NOT the base_url.
                            let norm_abs = abs.trim_end_matches('/');
                            let norm_base = base_url.trim_end_matches('/');
                            if norm_abs != norm_base {
                                results.push(abs);
                            }
                        }
                        in_response = false;
                        is_calendar = false;
                        current_href.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    results
}
