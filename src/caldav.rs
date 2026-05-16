// src/caldav.rs
use crate::config::Config;
use anyhow::Result;
use const_hex;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tracing::warn;
use uuid::Uuid;

pub struct CaldavClient {
    base: String,
    client: reqwest::Client,
}

impl CaldavClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .build()?;
        let base = Self::sanitize_base_url(&cfg.caldav_base);
        Ok(Self { base, client })
    }

    pub fn new_from_base(caldav_base: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(2)
            .pool_idle_timeout(Duration::from_secs(60))
            .tcp_keepalive(Duration::from_secs(30))
            .build()?;
        let base = Self::sanitize_base_url(caldav_base);
        Ok(Self { base, client })
    }

    /// Sanitize base URL by removing any embedded credentials.
    /// Credentials in the URL are deprecated and interfere with proper Basic Auth.
    /// Returns sanitized URL without userinfo, or original if parsing fails.
    fn sanitize_base_url(caldav_base: &str) -> String {
        match reqwest::Url::parse(caldav_base) {
            Ok(mut url) => {
                let had_creds = !url.username().is_empty() || url.password().is_some();
                if had_creds {
                    warn!(
                        "CalDAV base URL contains embedded credentials. These will be ignored; use GATEWAY_CALDAV_USER and GATEWAY_CALDAV_PASSWORD environment variables instead, or configure credentials separately. Sanitizing URL by removing userinfo."
                    );
                    url.set_username("").ok();
                    url.set_password(None).ok();
                    url.to_string()
                } else {
                    caldav_base.to_string()
                }
            }
            Err(_) => {
                // If URL is invalid, pass through unchanged; error will be caught elsewhere
                caldav_base.to_string()
            }
        }
    }

    pub async fn verify_credentials(&self, username: &str, password: &str) -> bool {
        matches!(
            self.verify_credentials_detailed(username, password).await,
            crate::auth::CaldavAuthResult::Valid
        )
    }

    /// Detailed credential verification that distinguishes between
    /// "wrong credentials" and "server unreachable".
    pub async fn verify_credentials_detailed(
        &self,
        username: &str,
        password: &str,
    ) -> crate::auth::CaldavAuthResult {
        let home_url = format!(
            "{}/cal/{}/",
            self.base.trim_end_matches('/'),
            urlencoding::encode(username)
        );
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop><D:resourcetype/></D:prop>
</D:propfind>"#;
        match self
            .client
            .request(
                reqwest::Method::from_bytes(b"PROPFIND").unwrap_or(reqwest::Method::GET),
                &home_url,
            )
            .basic_auth(username, Some(password))
            .header("Depth", "0")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(propfind_body)
            .send()
            .await
        {
            Ok(r) => {
                let status = r.status();
                if status.is_success() || status.as_u16() == 207 {
                    crate::auth::CaldavAuthResult::Valid
                } else if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    crate::auth::CaldavAuthResult::Invalid
                } else if status == reqwest::StatusCode::NOT_FOUND {
                    // 404: user authenticated but has no calendar home yet.
                    // Treat as valid so the gateway can provision on first sync.
                    crate::auth::CaldavAuthResult::Valid
                } else {
                    // 5xx or unexpected — treat as unreachable to avoid
                    // poisoning the auth cache on transient server errors.
                    tracing::warn!(
                        status = %status,
                        "CalDAV auth verification returned unexpected status; treating as unreachable"
                    );
                    crate::auth::CaldavAuthResult::Unreachable
                }
            }
            Err(e) => {
                if e.is_connect() || e.is_timeout() || e.is_request() {
                    tracing::warn!(
                        error = %e,
                        "CalDAV auth verification connection error; treating as unreachable"
                    );
                    crate::auth::CaldavAuthResult::Unreachable
                } else {
                    tracing::warn!(
                        error = %e,
                        "CalDAV auth verification unexpected error; treating as invalid"
                    );
                    crate::auth::CaldavAuthResult::Invalid
                }
            }
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub async fn get_freebusy(
        &self,
        collection_href: &str,
        start: &str,
        end: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let report = format!(
            r#"<?xml version="1.0" encoding="utf-8" ?>
<C:free-busy-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
<C:time-range start="{start}" end="{end}" />
</C:free-busy-query>"#,
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
            return Err(anyhow::anyhow!(
                "failed to query freebusy: {}",
                resp.status()
            ));
        }
        Ok(resp.text().await?)
    }

    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let username = username.to_string();
        let password = password.to_string();
        self.with_connect_retry(|| {
            let username = username.clone();
            let password = password.clone();
            async move { self.find_user_calendars_inner(&username, &password).await }
        })
        .await
    }

    async fn find_user_calendars_inner(
        &self,
        username: &str,
        password: &str,
    ) -> Result<Vec<String>> {
        let home_url = format!(
            "{}/cal/{}/",
            self.base.trim_end_matches('/'),
            urlencoding::encode(username)
        );

        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:resourcetype/>
    <D:displayname/>
  </D:prop>
</D:propfind>"#;

        let resp = match self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home_url)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(propfind_body)
            .send()
            .await
        {
            Ok(r) => Ok(r),
            Err(e) => {
                tracing::error!("caldav: PROPFIND request to {} failed: {}", home_url, e);
                Err(anyhow::anyhow!("CalDAV connection failed: {}", e))
            }
        }?;

        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::MULTI_STATUS {
            let body_preview = resp.text().await.unwrap_or_default();
            tracing::error!(
                "caldav: PROPFIND on {} returned status {}: {}",
                home_url,
                status,
                body_preview
            );
            return Err(anyhow::anyhow!(
                "CalDAV server returned {}: {}",
                status,
                if body_preview.len() > 200 {
                    "response truncated"
                } else {
                    &body_preview
                }
            ));
        }

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    "caldav: failed to read PROPFIND response body from {}: {}",
                    home_url,
                    e
                );
                return Err(anyhow::anyhow!("Failed to read CalDAV response: {}", e));
            }
        };

        let hrefs = parse_calendar_collection_hrefs(&body, &home_url);
        if hrefs.is_empty() {
            tracing::error!(
                "caldav: PROPFIND on {} returned no calendar collections. Check Stalwart configuration and user permissions.",
                home_url
            );
            return Err(anyhow::anyhow!("No calendar collections found for user"));
        }

        tracing::debug!(
            "caldav: discovered {} calendar collection(s) for {}",
            hrefs.len(),
            username
        );
        Ok(hrefs)
    }

    pub async fn query_events(
        &self,
        collection_href: &str,
        start: &str,
        end: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        let collection_href = collection_href.to_string();
        let start = start.to_string();
        let end = end.to_string();
        let username = username.to_string();
        let password = password.to_string();
        self.with_connect_retry(|| {
            let collection_href = collection_href.clone();
            let start = start.clone();
            let end = end.clone();
            let username = username.clone();
            let password = password.clone();
            async move {
                self.query_events_inner(&collection_href, &start, &end, &username, &password)
                    .await
            }
        })
        .await
    }

    async fn query_events_inner(
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

        let resp = match self
            .client
            .request(reqwest::Method::from_bytes(b"REPORT")?, collection_href)
            .basic_auth(username, Some(password))
            .header("Content-Type", "application/xml; charset=utf-8")
            .header("Depth", "1")
            .body(report)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(
                    "caldav: REPORT request to {} failed: {}",
                    collection_href,
                    e
                );
                return Err(anyhow::anyhow!("CalDAV connection failed: {}", e));
            }
        };

        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::MULTI_STATUS {
            let body_preview = resp.text().await.unwrap_or_default();
            tracing::error!(
                "caldav: REPORT on {} returned status {}: {}",
                collection_href,
                status,
                if body_preview.len() > 500 {
                    "response truncated"
                } else {
                    &body_preview
                }
            );
            return Err(anyhow::anyhow!(
                "CalDAV server returned {}: {}",
                status,
                if body_preview.len() > 200 {
                    "response truncated"
                } else {
                    &body_preview
                }
            ));
        }

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                tracing::error!(
                    "caldav: failed to read REPORT response body from {}: {}",
                    collection_href,
                    e
                );
                return Err(anyhow::anyhow!("Failed to read CalDAV response: {}", e));
            }
        };

        Ok(body)
    }

    pub async fn get_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
    ) -> Result<(String, Option<String>)> {
        let resource_href = resource_href.to_string();
        let username = username.to_string();
        let password = password.to_string();
        self.with_connect_retry(|| {
            let resource_href = resource_href.clone();
            let username = username.clone();
            let password = password.clone();
            async move { self.get_event_inner(&resource_href, &username, &password).await }
        })
        .await
    }

    async fn get_event_inner(
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
        let result = self.put_event_inner(collection_href, resource_href, ics, username, password, if_match).await;
        if let Err(ref e) = result {
            let err_msg = format!("{}", e);
            if err_msg.contains("412") {
                tracing::warn!(
                    "CalDAV PUT returned 412 Precondition Failed, retrying without If-Match"
                );
                return self.put_event_inner(collection_href, resource_href, ics, username, password, None).await;
            }
        }
        result
    }

    async fn put_event_inner(
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
        let resource_href_out = self.relative_href(&target);
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.trim_matches('"').to_string());

        let etag = match etag {
            Some(e) => e,
            None => {
                // Stalwart may not return ETag in PUT response (some configs).
                // Fetch the actual ETag via GET to avoid synthetic-etag mismatch
                // on the next update (which would cause 412 Precondition Failed).
                tracing::debug!(
                    href = %resource_href_out,
                    "PUT response missing ETag; fetching actual ETag via GET"
                );
                match self
                    .get_event(&resource_href_out, username, password)
                    .await
                {
                    Ok((_, Some(real_etag))) => real_etag,
                    _ => {
                        tracing::warn!(
                            href = %resource_href_out,
                            "Failed to fetch ETag after PUT; falling back to synthetic etag"
                        );
                        self.synthetic_etag(ics)
                    }
                }
            }
        };
        Ok((resource_href_out, etag))
    }

    pub async fn delete_event(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
        if_match: Option<&str>,
    ) -> Result<()> {
        let result = self.delete_event_inner(resource_href, username, password, if_match).await;
        if let Err(ref e) = result {
            let err_msg = format!("{}", e);
            if err_msg.contains("412") {
                tracing::warn!(
                    "CalDAV DELETE returned 412 Precondition Failed, retrying without If-Match"
                );
                return self.delete_event_inner(resource_href, username, password, None).await;
            }
        }
        result
    }

    async fn delete_event_inner(
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
        const_hex::encode(Sha256::digest(ics.as_bytes()))
    }

    /// Execute an async CalDAV operation with one retry on connection errors.
    /// Stalwart may briefly become unavailable during restarts; one retry with
    /// a 500ms backoff covers the common case.
    async fn with_connect_retry<F, Fut, T>(&self, op: F) -> Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        match op().await {
            Ok(v) => Ok(v),
            Err(e) => {
                let err_str = format!("{}", e);
                let is_connect = err_str.contains("connection")
                    || err_str.contains("timed out")
                    || err_str.contains("connect")
                    || err_str.contains("dns")
                    || err_str.contains("refused");
                if is_connect {
                    warn!(
                        error = %e,
                        "CalDAV connection error; retrying after 500ms backoff"
                    );
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    op().await
                } else {
                    Err(e)
                }
            }
        }
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
