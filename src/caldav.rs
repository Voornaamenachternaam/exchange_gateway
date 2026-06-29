// src/caldav.rs
use crate::config::Config;
use anyhow::{Result, anyhow};
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
            .build()?;
        let base = Self::sanitize_base_url(&cfg.caldav_base);
        Ok(Self { base, client })
    }

    pub fn new_from_base(caldav_base: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
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
        let home_url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);
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
            Ok(r) => r.status().is_success() || r.status().as_u16() == 207,
            Err(_) => false,
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
        let home_url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);

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
            .map(normalize_etag_to_internal);
        Ok((resp.text().await?, etag))
    }

    /// Fetch the current server-side ETag for a CalDAV resource via PROPFIND.
    /// This is needed because Stalwart v0.16.5 may not return an ETag header on GET,
    /// but always includes it in PROPFIND/REPORT multistatus responses.
    pub async fn get_etag(
        &self,
        resource_href: &str,
        username: &str,
        password: &str,
    ) -> Result<Option<String>> {
        let url = self.absolute_url(resource_href)?;
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop><D:getetag/></D:prop>
</D:propfind>"#;
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &url)
            .basic_auth(username, Some(password))
            .header("Depth", "0")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(propfind_body)
            .send()
            .await?;
        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Ok(None);
        }
        let body = resp.text().await?;
        Ok(parse_etag_from_multistatus(&body))
    }

    /// Move a calendar event from one collection to another using CalDAV MOVE.
    /// `src_href` is the source resource path (e.g., "event123.ics").
    /// `src_collection_href` is the source collection (calendar) path.
    /// `dst_collection_href` is the destination collection path.
    /// Returns new etag for the moved resource.
    pub async fn move_event(
        &self,
        src_href: &str,
        src_collection_href: &str,
        dst_collection_href: &str,
        username: &str,
        password: &str,
    ) -> Result<String> {
        // Construct full source URL
        let src_url = self.resolve_resource_url(src_collection_href, Some(src_href))?;
        // Destination URL: must be absolute per RFC 4918 Section 10.3
        let dst_url = self.resolve_resource_url(dst_collection_href, Some(src_href))?;

        // Build Destination header
        let req = self
            .client
            .request(reqwest::Method::from_bytes(b"MOVE")?, &src_url)
            .basic_auth(username, Some(password))
            .header("Destination", &dst_url);

        // We can optionally include Overwrite: T (default) or F, but not needed.
        let resp = req.send().await?;
        let status = resp.status();

        if !status.is_success() {
            let body_preview = resp.text().await.unwrap_or_default();
            return Err(anyhow!("CalDAV MOVE failed: {} - {}", status, body_preview));
        }

        // After move, the resource is at the new destination. We need its new ETag.
        // The response may include ETag header directly; otherwise we can fetch via PROPFIND on destination.
        if let Some(etag_val) = resp.headers().get("ETag")
            .and_then(|v| v.to_str().ok())
        {
            Ok(normalize_etag_to_internal(etag_val))
        } else {
            // Fallback: fetch the ETag from the new location
            let etag_opt = self.get_etag(&dst_url, username, password).await?;
            etag_opt.ok_or_else(|| anyhow!("Moved resource not found at destination for ETag retrieval"))
        }
    }

    /// Get the primary calendar collection href for a user.
    /// For Stalwart, this is typically "/cal/{username}/".
    pub fn calendar_collection_href(&self, username: &str) -> String {
        format!("/cal/{}/", username)
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

        // Only use If-Match with a server-recognized etag (not a synthetic one).
        // Synthetic etags are prefixed with "sgw-" by this gateway, or "W/" (weak).
        // Sending a synthetic etag would cause Stalwart v0.16.5 to return
        // 412 Precondition Failed.
        let valid_if_match = if_match.filter(|e| !Self::is_synthetic_etag(e));
        let mut req = self
            .client
            .put(&target)
            .basic_auth(username, Some(password))
            .header(CONTENT_TYPE, "text/calendar; charset=utf-8")
            .body(ics.to_string());

        if let Some(etag) = valid_if_match {
            req = req.header(IF_MATCH, Self::format_etag_for_if_match(etag));
        } else if resource_href.is_none() {
            req = req.header(IF_NONE_MATCH, "*");
        }

        let resp = req.send().await?;
        let status = resp.status();

        if status == reqwest::StatusCode::PRECONDITION_FAILED {
            // 412: If-Match etag was stale. Refresh the etag via PROPFIND and retry
            // without If-Match (unconditional overwrite) to avoid client-facing errors.
            // This handles the case where the stored etag is outdated because another
            // client or device updated the event between our last sync and this update.
            warn!(
                target = %target,
                "CalDAV PUT returned 412 Precondition Failed; refreshing etag and retrying unconditionally"
            );
            if let Some(refreshed_etag) = self
                .get_etag(resource_href.unwrap_or(&target), username, password)
                .await
                .ok()
                .flatten()
            {
                // Per RFC 7232 §3.1, If-Match uses the strong comparison function.
                // Weak etags (W/...) cannot match in a strong comparison, so sending
                // them in If-Match would cause another 412 or 400 on strict servers.
                // Synthetic etags (sgw-...) are gateway-generated and not recognized
                // by the CalDAV server, so they would also cause 412.
                // In both cases, skip the conditional retry and fall through to the
                // unconditional PUT fallback to avoid a wasteful round-trip.
                if Self::is_synthetic_etag(&refreshed_etag) {
                    warn!(
                        target = %target,
                        refreshed_etag = %refreshed_etag,
                        "Refreshed etag is weak/synthetic; skipping conditional retry, falling back to unconditional PUT"
                    );
                } else {
                    tracing::info!(target = %target, refreshed_etag = %refreshed_etag, "Refreshed etag from PROPFIND; retrying PUT with If-Match");
                    let retry = self
                        .client
                        .put(&target)
                        .basic_auth(username, Some(password))
                        .header(CONTENT_TYPE, "text/calendar; charset=utf-8")
                        .header(IF_MATCH, Self::format_etag_for_if_match(&refreshed_etag))
                        .body(ics.to_string())
                        .send()
                        .await?;
                    if retry.status().is_success() {
                        let etag = retry
                            .headers()
                            .get(ETAG)
                            .and_then(|v| v.to_str().ok())
                            .map(normalize_etag_to_internal)
                            .unwrap_or_else(|| self.synthetic_etag(ics));
                        return Ok((self.relative_href(&target), etag));
                    }
                    // If retry with refreshed etag also fails, fall through to unconditional
                    warn!(target = %target, retry_status = %retry.status(), "Retry with refreshed etag failed; falling back to unconditional PUT");
                }
            }
            // Final fallback: unconditional PUT without If-Match
            let fallback = self
                .client
                .put(&target)
                .basic_auth(username, Some(password))
                .header(CONTENT_TYPE, "text/calendar; charset=utf-8")
                .body(ics.to_string())
                .send()
                .await?;
            if !fallback.status().is_success() {
                return Err(anyhow::anyhow!(
                    "failed to write event after 412 retry: {}",
                    fallback.status()
                ));
            }
            let etag = fallback
                .headers()
                .get(ETAG)
                .and_then(|v| v.to_str().ok())
                .map(normalize_etag_to_internal)
                .unwrap_or_else(|| self.synthetic_etag(ics));
            return Ok((self.relative_href(&target), etag));
        }

        if !status.is_success() {
            return Err(anyhow::anyhow!("failed to write event: {}", status));
        }
        let etag = resp
            .headers()
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(normalize_etag_to_internal)
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
        // Only use If-Match with a server-recognized etag (not a synthetic one).
        let valid_if_match = if_match.filter(|e| !Self::is_synthetic_etag(e));
        let mut req = self.client.delete(url).basic_auth(username, Some(password));
        if let Some(etag) = valid_if_match {
            req = req.header(IF_MATCH, Self::format_etag_for_if_match(etag));
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::PRECONDITION_FAILED {
            // 412 on delete: etag is stale. Retry without If-Match.
            warn!(
                resource_href = %resource_href,
                "CalDAV DELETE returned 412; retrying unconditionally"
            );
            let url2 = self.absolute_url(resource_href)?;
            let retry = self
                .client
                .delete(url2)
                .basic_auth(username, Some(password))
                .send()
                .await?;
            if !retry.status().is_success() {
                return Err(anyhow::anyhow!(
                    "failed to delete event after 412 retry: {}",
                    retry.status()
                ));
            }
            return Ok(());
        }
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

    /// Prefix used to mark synthetic ETags generated by this gateway.
    /// Server-issued ETags will never start with this prefix, so we can
    /// reliably filter them out before sending If-Match headers.
    pub const SYNTHETIC_ETAG_PREFIX: &str = "sgw-";

    fn synthetic_etag(&self, ics: &str) -> String {
        format!(
            "{}{}",
            Self::SYNTHETIC_ETAG_PREFIX,
            const_hex::encode(Sha256::digest(ics.as_bytes()))
        )
    }

    /// Returns true if the etag was generated by this gateway (synthetic)
    /// and would not be recognized by the CalDAV server.
    fn is_synthetic_etag(etag: &str) -> bool {
        etag.starts_with(Self::SYNTHETIC_ETAG_PREFIX) || etag.starts_with("W/")
    }

    /// Format an etag value for use in an If-Match HTTP header per RFC 7232 §2.3.
    ///
    /// Entity-tags in conditional headers MUST be enclosed in DQUOTE:
    /// `If-Match: "etag_value"` (strong)
    /// `If-Match: W/"etag_value"` (weak)
    ///
    /// Internally, etags are stored without surrounding quotes (stripped by
    /// `normalize_etag_to_internal`). This function re-adds the required
    /// quotes. It is fully idempotent: any valid input format produces the
    /// same correct output, including already-quoted or weak-prefixed values.
    fn format_etag_for_if_match(etag: &str) -> String {
        // Strip any existing W/ prefix, then strip any surrounding quotes,
        // then re-format per RFC 7232 §2.3:  [W/]"<opaque-tag>"
        let (is_weak, rest) = if let Some(s) = etag.strip_prefix("W/") {
            (true, s)
        } else {
            (false, etag)
        };
        let opaque = rest.trim_matches('"');
        if is_weak {
            format!("W/\"{}\"", opaque)
        } else {
            format!("\"{}\"", opaque)
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

/// Parse the D:getetag value from a WebDAV PROPFIND multistatus response.
/// Returns None if parsing fails or no etag is found.
///
/// The returned etag is stored internally as the bare opaque-tag with an
/// optional `W/` prefix for weak etags, but **without** surrounding DQUOTE
/// characters. For example:
///   `"1419368738"` → `1419368738`
///   `W/"123abc"`   → `W/123abc`
fn parse_etag_from_multistatus(xml_body: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(xml_body);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_getetag = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(ref e)) => {
                let local = e.name().local_name();
                if local.as_ref() == b"getetag" {
                    in_getetag = true;
                }
            }
            Ok(quick_xml::events::Event::End(ref e)) => {
                let local = e.name().local_name();
                if local.as_ref() == b"getetag" {
                    in_getetag = false;
                }
            }
            Ok(quick_xml::events::Event::Text(ref t)) if in_getetag => {
                if let Ok(text) = t.decode() {
                    let etag = normalize_etag_to_internal(&text);
                    if !etag.is_empty() {
                        return Some(etag);
                    }
                }
            }
            Ok(quick_xml::events::Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    None
}

/// Normalize an etag string to its internal (unquoted) representation.
///
/// Strips the optional `W/` weak prefix, removes surrounding DQUOTE
/// characters from the opaque-tag, then re-attaches the `W/` prefix if
/// the original was weak. This handles all edge cases:
///
/// | Input          | Internal  |
/// |----------------|-----------|
/// | `"123"`        | `123`     |
/// | `123`          | `123`     |
/// | `W/"123"`      | `W/123`   |
/// | `W/123`        | `W/123`   |
/// | `W/""123""`    | `W/123`   |
fn normalize_etag_to_internal(raw: &str) -> String {
    let (is_weak, rest) = if let Some(s) = raw.strip_prefix("W/") {
        (true, s)
    } else {
        (false, raw)
    };
    let opaque = rest.trim_matches('"');
    if is_weak {
        format!("W/{}", opaque)
    } else {
        opaque.to_string()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_etag_for_if_match_plain() {
        // Standard numeric etag from Stalwart PROPFIND (stripped of quotes by parser)
        assert_eq!(
            CaldavClient::format_etag_for_if_match("1419368738"),
            "\"1419368738\""
        );
    }

    #[test]
    fn test_format_etag_for_if_match_hex() {
        // Hex etag as seen in MS-XWDCAL examples
        assert_eq!(
            CaldavClient::format_etag_for_if_match("1c5a707ee8157a47bfce2b746a3dba250000012c30ab"),
            "\"1c5a707ee8157a47bfce2b746a3dba250000012c30ab\""
        );
    }

    #[test]
    fn test_format_etag_for_if_match_already_quoted() {
        // Idempotent: already-quoted strong etag → same output
        assert_eq!(
            CaldavClient::format_etag_for_if_match("\"1419368738\""),
            "\"1419368738\""
        );
    }

    #[test]
    fn test_format_etag_for_if_match_weak_unquoted() {
        // Weak etag stored internally as W/1419368738 (after normalize_etag_to_internal)
        assert_eq!(
            CaldavClient::format_etag_for_if_match("W/1419368738"),
            "W/\"1419368738\""
        );
    }

    #[test]
    fn test_format_etag_for_if_match_weak_already_quoted() {
        // Idempotent: W/"1419368738" (already quoted weak etag) → same output
        // This was the original bug: old code produced W/""1419368738""
        assert_eq!(
            CaldavClient::format_etag_for_if_match("W/\"1419368738\""),
            "W/\"1419368738\""
        );
    }

    #[test]
    fn test_format_etag_for_if_match_empty() {
        // Empty etag (shouldn't happen, but be safe)
        assert_eq!(CaldavClient::format_etag_for_if_match(""), "\"\"");
    }

    #[test]
    fn test_normalize_etag_to_internal_strong_quoted() {
        // Server returns: "1419368738"  →  internal: 1419368738
        assert_eq!(normalize_etag_to_internal("\"1419368738\""), "1419368738");
    }

    #[test]
    fn test_normalize_etag_to_internal_strong_unquoted() {
        // Already unquoted: 1419368738  →  internal: 1419368738
        assert_eq!(normalize_etag_to_internal("1419368738"), "1419368738");
    }

    #[test]
    fn test_normalize_etag_to_internal_weak_quoted() {
        // Server returns: W/"123"  →  internal: W/123
        // This was the original bug: trim_matches('"') on W/"123" produced W/"123
        assert_eq!(normalize_etag_to_internal("W/\"123\""), "W/123");
    }

    #[test]
    fn test_normalize_etag_to_internal_weak_unquoted() {
        // Already normalized: W/123  →  internal: W/123
        assert_eq!(normalize_etag_to_internal("W/123"), "W/123");
    }

    #[test]
    fn test_normalize_etag_to_internal_double_quoted() {
        // Edge case: W/""123""  →  internal: W/123
        assert_eq!(normalize_etag_to_internal("W/\"\"123\"\""), "W/123");
    }

    #[test]
    fn test_parse_etag_and_format_roundtrip() {
        // Simulate full roundtrip: PROPFIND XML → parse → format for If-Match
        let propfind_response = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/cal/user/default/event.ics</D:href>
    <D:propstat>
      <D:prop><D:getetag>"1419368738"</D:getetag></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let parsed = parse_etag_from_multistatus(propfind_response).unwrap();
        assert_eq!(parsed, "1419368738"); // Internal: unquoted
        let if_match_value = CaldavClient::format_etag_for_if_match(&parsed);
        assert_eq!(if_match_value, "\"1419368738\""); // If-Match: RFC 7232 quoted
    }

    #[test]
    fn test_parse_etag_with_quotes_and_format_roundtrip() {
        // Stalwart sometimes returns hex etags with inner quotes in PROPFIND
        let propfind_response = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/cal/user/default/event.ics</D:href>
    <D:propstat>
      <D:prop><D:getetag>"1c5a707ee8157a47bfce2b746a3dba250000012c30ab"</D:getetag></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let parsed = parse_etag_from_multistatus(propfind_response).unwrap();
        assert_eq!(parsed, "1c5a707ee8157a47bfce2b746a3dba250000012c30ab");
        let if_match_value = CaldavClient::format_etag_for_if_match(&parsed);
        assert_eq!(
            if_match_value,
            "\"1c5a707ee8157a47bfce2b746a3dba250000012c30ab\""
        );
    }

    #[test]
    fn test_parse_weak_etag_and_format_roundtrip() {
        // Weak etag roundtrip: PROPFIND returns W/"123" → parse → format for If-Match
        let propfind_response = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/cal/user/default/event.ics</D:href>
    <D:propstat>
      <D:prop><D:getetag>W/"abc123"</D:getetag></D:prop>
      <D:status>HTTP/1.1 200 OK</D:status>
    </D:propstat>
  </D:response>
</D:multistatus>"#;
        let parsed = parse_etag_from_multistatus(propfind_response).unwrap();
        assert_eq!(parsed, "W/abc123"); // Internal: W/ prefix + bare opaque-tag
        let if_match_value = CaldavClient::format_etag_for_if_match(&parsed);
        assert_eq!(if_match_value, "W/\"abc123\""); // If-Match: RFC 7232 weak format
    }

    #[test]
    fn test_is_synthetic_etag_detects_weak() {
        // Weak etags should be treated as synthetic (not usable in If-Match)
        assert!(CaldavClient::is_synthetic_etag("W/123"));
        assert!(CaldavClient::is_synthetic_etag("W/\"123\""));
    }

    #[test]
    fn test_is_synthetic_etag_detects_gateway_prefix() {
        // Gateway-generated etags should be treated as synthetic
        assert!(CaldavClient::is_synthetic_etag("sgw-abc123"));
    }

    #[test]
    fn test_is_synthetic_etag_allows_strong() {
        // Strong server-issued etags should NOT be treated as synthetic
        assert!(!CaldavClient::is_synthetic_etag("1419368738"));
        assert!(!CaldavClient::is_synthetic_etag(
            "1c5a707ee8157a47bfce2b746a3dba250000012c30ab"
        ));
    }

    #[test]
    fn test_format_etag_idempotency_all_forms() {
        // Regardless of input format, output must be the same (idempotent)
        let expected_strong = "\"1419368738\"";
        assert_eq!(
            CaldavClient::format_etag_for_if_match("1419368738"),
            expected_strong
        );
        assert_eq!(
            CaldavClient::format_etag_for_if_match("\"1419368738\""),
            expected_strong
        );

        let expected_weak = "W/\"1419368738\"";
        assert_eq!(
            CaldavClient::format_etag_for_if_match("W/1419368738"),
            expected_weak
        );
        assert_eq!(
            CaldavClient::format_etag_for_if_match("W/\"1419368738\""),
            expected_weak
        );
    }
}
