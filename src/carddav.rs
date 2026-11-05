// src/carddav.rs
use crate::config::Config;
use anyhow::{Result, anyhow};
use reqwest::{Client, StatusCode};
use std::time::Duration;
use tracing::warn;

/// CardDAV client for contacts sync.
///
/// Stalwart Mailserver serves CardDAV under `/dav/card/{user}/` (the
/// addressbook home set), so `base` is the CardDAV root
/// (`http://host:port/dav/card`) and [`Self::addressbook_home`] appends the
/// per-user segment.
pub struct CarddavClient {
    pub base: String,
    pub client: Client,
}

/// Represents a contact entry from CardDAV.
#[derive(Debug, Clone)]
pub struct Contact {
    pub href: String,
    pub etag: Option<String>,
    pub vcard: String,
}

impl CarddavClient {
    /// Construct a new CardDAV client from configuration.
    pub fn new(cfg: &Config) -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(30)).build()?;
        let base = Self::sanitize_base_url(&cfg.carddav_base);
        Ok(Self { base, client })
    }

    /// Derive CardDAV base from CalDAV base if not explicitly configured.
    /// Stalwart serves CardDAV under `/dav/card` next to CalDAV's `/dav/cal`,
    /// so a CalDAV base of `http://host:port/dav` (or `.../dav/`) maps to
    /// `http://host:port/dav/card`. Any other shape keeps its path and gains
    /// a `/card` segment.
    pub fn from_caldav_base(caldav_base: &str) -> Self {
        let base = Self::derive_base_from_caldav(caldav_base);
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build CardDAV client");
        Self { base, client }
    }

    /// Pure derivation of the CardDAV base from a CalDAV base, used by both
    /// the config loader and [`Self::from_caldav_base`].
    ///
    /// Stalwart serves CalDAV at `{root}/dav/cal/{user}/` and CardDAV at
    /// `{root}/dav/card/{user}/`, so the CardDAV base is the CalDAV base with
    /// any calendar sub-path dropped and `/card` appended
    /// (`http://host/dav` → `http://host/dav/card`,
    /// `http://host/dav/cal` → `http://host/dav/card`).
    ///
    /// The match runs against the URL *path* only, so a host or port that
    /// contains `dav` (e.g. `https://dav.example.com/…`) is never mistaken for
    /// the DAV root.
    pub fn derive_base_from_caldav(caldav_base: &str) -> String {
        let trimmed = caldav_base.trim_end_matches('/');
        if trimmed.is_empty() {
            return String::new();
        }
        let (origin, path) = Self::split_origin_and_path(trimmed);
        let path = path.trim_end_matches('/');
        // A `/dav` segment is the DAV root; anything after it (e.g. `/cal`) is a
        // collection sub-path that must be dropped.
        let new_path = match path.rfind("/dav") {
            Some(idx)
                if path[idx + "/dav".len()..].is_empty()
                    || path[idx + "/dav".len()..].starts_with('/') =>
            {
                format!("{}/card", &path[..idx + "/dav".len()])
            }
            _ => format!("{path}/card"),
        };
        format!("{origin}{new_path}")
    }

    /// Split a URL-ish string into its origin (`scheme://host[:port]`) and its
    /// path. Falls back to a scheme-relative split when the value is not a
    /// parseable absolute URL.
    fn split_origin_and_path(value: &str) -> (String, String) {
        if let Ok(url) = reqwest::Url::parse(value) {
            let origin = match url.port() {
                Some(port) => format!(
                    "{}://{}:{}",
                    url.scheme(),
                    url.host_str().unwrap_or(""),
                    port
                ),
                None => format!("{}://{}", url.scheme(), url.host_str().unwrap_or("")),
            };
            return (origin, url.path().to_string());
        }
        let scheme_end = value.find("://").map(|i| i + 3).unwrap_or(0);
        match value[scheme_end..].find('/') {
            Some(rel) => {
                let split = scheme_end + rel;
                (value[..split].to_string(), value[split..].to_string())
            }
            None => (value.to_string(), String::new()),
        }
    }

    /// Sanitize base URL by removing embedded credentials.
    fn sanitize_base_url(carddav_base: &str) -> String {
        match reqwest::Url::parse(carddav_base) {
            Ok(mut url) => {
                let had_creds = !url.username().is_empty() || url.password().is_some();
                if had_creds {
                    warn!(
                        "CardDAV base URL contains embedded credentials; these will be ignored. Sanitizing URL."
                    );
                    url.set_username("").ok();
                    url.set_password(None).ok();
                    url.to_string()
                } else {
                    carddav_base.to_string()
                }
            }
            Err(_) => carddav_base.to_string(),
        }
    }

    /// Build the addressbook home set URL for a given user:
    /// `{base}/{username}/` where `base` is the CardDAV root
    /// (`…/dav/card`), matching Stalwart's `/dav/card/{user}/` home.
    pub fn addressbook_home(&self, username: &str) -> String {
        format!("{}/{}/", self.base.trim_end_matches('/'), username)
    }

    /// Verify credentials by performing a PROPFIND on the addressbook home.
    pub async fn verify_credentials(&self, username: &str, password: &str) -> bool {
        let home_url = self.addressbook_home(username);
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
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
            .body(body)
            .send()
            .await
        {
            Ok(r) => r.status().is_success() || r.status().as_u16() == 207,
            Err(_) => false,
        }
    }

    /// Fetch a single contact vCard by its href (relative to addressbook home).
    pub async fn get_contact(
        &self,
        username: &str,
        password: &str,
        href: &str,
    ) -> Result<(String, Option<String>)> {
        let home = self.addressbook_home(username);
        let url = format!("{}{}", home, href);
        let resp = self
            .client
            .get(&url)
            .basic_auth(username, Some(password))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("CardDAV GET failed: {}", resp.status()));
        }
        let etag = resp
            .headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        Ok((resp.text().await?, etag))
    }

    /// Create or update a contact.
    /// `href` is the filename (e.g., "contact123.vcf"); if None, server will assign a new one.
    /// Returns the final href and etag.
    pub async fn put_contact(
        &self,
        username: &str,
        password: &str,
        href: Option<&str>,
        vcard: &str,
    ) -> Result<(String, String)> {
        let home = self.addressbook_home(username);
        let url = match href {
            Some(h) => format!("{}{}", home, h),
            None => home.trim_end_matches('/').to_string(),
        };
        let resp = self
            .client
            .put(&url)
            .basic_auth(username, Some(password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .body(vcard.to_string())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() && status != reqwest::StatusCode::CREATED {
            return Err(anyhow!("CardDAV PUT failed: {}", status));
        }
        // Extract Location header for new resource if created
        let final_href = if let Some(loc) = resp.headers().get("Location") {
            // Extract path portion
            let loc_url = loc
                .to_str()
                .map_err(|_| anyhow!("Invalid Location header"))?;
            // Get the part after the home URL
            loc_url.strip_prefix(&home).unwrap_or(loc_url).to_string()
        } else {
            // If no Location, use provided href or empty string
            href.unwrap_or("").to_string()
        };
        let etag = resp
            .headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "".to_string());
        Ok((final_href, etag))
    }

    /// Delete a contact by its href.
    pub async fn delete_contact(&self, username: &str, password: &str, href: &str) -> Result<()> {
        let home = self.addressbook_home(username);
        let url = format!("{}{}", home, href);
        let resp = self
            .client
            .delete(&url)
            .basic_auth(username, Some(password))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("CardDAV DELETE failed: {}", resp.status()));
        }
        Ok(())
    }

    /// Create a new contact by POSTing a vCard to the addressbook home.
    /// Returns (href, etag). href is relative to addressbook home.
    pub async fn create_contact(
        &self,
        username: &str,
        password: &str,
        vcard: &str,
    ) -> Result<(String, String)> {
        let home = self.addressbook_home(username);
        let resp = self
            .client
            .post(&home)
            .basic_auth(username, Some(password))
            .header("Content-Type", "text/vcard; charset=utf-8")
            .body(vcard.to_string())
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() && status != StatusCode::CREATED {
            return Err(anyhow!("CardDAV POST failed: {}", status));
        }
        // Location header gives absolute URL; extract path relative to home
        let location = resp
            .headers()
            .get("Location")
            .ok_or_else(|| anyhow!("Missing Location header in create response"))?;
        let loc_str = location
            .to_str()
            .map_err(|_| anyhow!("Invalid Location header"))?;
        let href = loc_str
            .strip_prefix(&home)
            .unwrap_or(loc_str)
            .trim_end_matches('/')
            .to_string();
        // ETag header (may include quotes)
        let etag = resp
            .headers()
            .get("ETag")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        Ok((href, etag))
    }

    /// List contacts from the addressbook.
    /// Returns (contacts vec, sync_token). sync_token can be used for later sync queries.
    /// Fetch all contacts from the addressbook using a single addressbook-query REPORT.
    /// This is much more efficient than PROPFIND + individual GETs per contact.
    pub async fn list_contacts(
        &self,
        username: &str,
        password: &str,
        _sync_token: Option<&str>,
    ) -> Result<(Vec<Contact>, Option<String>)> {
        let home = self.addressbook_home(username);

        // Use CardDAV addressbook-query REPORT to fetch all vCards in a single request.
        // This returns <address-data> containing full vCard for each contact.
        let query_body = r#"<?xml version="1.0"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
</C:addressbook-query>"#;

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"REPORT")?, &home)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "text/xml")
            .body(query_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            // If REPORT is not supported, fall back to PROPFIND+multiget
            return self.list_contacts_fallback(username, password).await;
        }

        let xml = resp.text().await?;
        let doc =
            roxmltree::Document::parse(&xml).map_err(|e| anyhow!("XML parse error: {}", e))?;

        let mut contacts = Vec::new();
        let mut new_sync_token = None;

        // Process each <D:response> element containing a contact
        for resp_elem in doc.descendants().filter(|n| n.has_tag_name("response")) {
            let href_elem = resp_elem.descendants().find(|n| n.has_tag_name("href"));
            let etag_elem = resp_elem.descendants().find(|n| n.has_tag_name("getetag"));
            let addr_elem = resp_elem
                .descendants()
                .find(|n| n.has_tag_name("address-data"));

            if let (Some(href_node), Some(etag_node)) = (href_elem, etag_elem) {
                let href = href_node.text().unwrap_or("").trim().to_string();
                let etag = etag_node.text().unwrap_or("").trim().to_string();

                // Extract vCard from <address-data> if present, otherwise will fetch via GET later
                let vcard = if let Some(addr_node) = addr_elem {
                    // address-data may contain CDATA or direct text; concatenate all text nodes
                    addr_node
                        .text()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };

                // Only include contacts (skip collection itself)
                if !href.is_empty() && etag != "\"\"" && !href.ends_with('/') {
                    // If vCard missing from REPORT, fetch it separately (shouldn't happen with Stalwart)
                    let vcard = if let Some(vc) = vcard {
                        vc
                    } else {
                        // Fallback: GET the vCard (rare)
                        let contact_url = format!("{}{}", home, href);
                        match self
                            .client
                            .get(&contact_url)
                            .basic_auth(username, Some(password))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                resp.text().await.unwrap_or_default()
                            }
                            _ => continue,
                        }
                    };

                    contacts.push(Contact {
                        href,
                        etag: Some(etag),
                        vcard,
                    });
                }
            }

            // Capture sync token if available (only first occurrence)
            if new_sync_token.is_none()
                && let Some(etag_elem) = resp_elem
                    .descendants()
                    .find(|n| n.has_tag_name("sync-token"))
                && let Some(tok) = etag_elem.text()
            {
                new_sync_token = Some(tok.trim().to_string());
            }
        }

        Ok((contacts, new_sync_token))
    }

    /// Fallback implementation using PROPFIND + individual GETs.
    /// Retained for compatibility with CardDAV servers that don't support addressbook-query.
    async fn list_contacts_fallback(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(Vec<Contact>, Option<String>)> {
        let home = self.addressbook_home(username);
        // PROPFIND body to request etags only
        let body = r#"<?xml version="1.0"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:propfind>"#;

        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "text/xml")
            .body(body)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(anyhow!("CardDAV PROPFIND failed: {}", resp.status()));
        }

        let xml = resp.text().await?;
        let doc =
            roxmltree::Document::parse(&xml).map_err(|e| anyhow!("XML parse error: {}", e))?;

        let mut contacts = Vec::new();
        let new_sync_token = None;

        // Collect hrefs/etags first
        let mut items = Vec::new();
        for resp_elem in doc.descendants().filter(|n| n.has_tag_name("response")) {
            let href_elem = resp_elem.descendants().find(|n| n.has_tag_name("href"));
            let etag_elem = resp_elem.descendants().find(|n| n.has_tag_name("getetag"));

            if let (Some(href_node), Some(etag_node)) = (href_elem, etag_elem) {
                let href = href_node.text().unwrap_or("").trim().to_string();
                let etag = etag_node.text().unwrap_or("").trim().to_string();
                if !href.is_empty() && etag != "\"\"" && !href.ends_with('/') {
                    items.push((href, etag));
                }
            }
        }

        // Batch fetch vCards in a single multiget REPORT for remaining items
        // (More efficient than individual GETs)
        if !items.is_empty() {
            // Build a multi-get REPORT using <address-data> requests
            let report_body = r#"<?xml version="1.0"?>
<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:getetag/>
    <C:address-data/>
  </D:prop>
  <C:filter/>
</C:addressbook-query>"#
                .to_string();

            let resp = self
                .client
                .request(reqwest::Method::from_bytes(b"REPORT")?, &home)
                .basic_auth(username, Some(password))
                .header("Depth", "1")
                .header("Content-Type", "text/xml")
                .body(report_body)
                .send()
                .await?;

            if resp.status().is_success()
                && let Ok(xml) = resp.text().await
                && let Ok(doc) = roxmltree::Document::parse(&xml)
            {
                for resp_elem in doc.descendants().filter(|n| n.has_tag_name("response")) {
                    let href_elem = resp_elem.descendants().find(|n| n.has_tag_name("href"));
                    let etag_elem = resp_elem.descendants().find(|n| n.has_tag_name("getetag"));
                    let addr_elem = resp_elem
                        .descendants()
                        .find(|n| n.has_tag_name("address-data"));

                    if let (Some(href_node), Some(etag_node)) = (href_elem, etag_elem) {
                        let href = href_node.text().unwrap_or("").trim().to_string();
                        let etag = etag_node.text().unwrap_or("").trim().to_string();
                        let vcard = addr_elem
                            .and_then(|n| n.text())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if !vcard.is_empty() && !href.is_empty() && etag != "\"\"" {
                            contacts.push(Contact {
                                href,
                                etag: Some(etag),
                                vcard,
                            });
                        }
                    }
                }
                return Ok((contacts, new_sync_token));
            }
        }

        // If REPORT fails, fall back to individual GETs
        for (href, etag) in items {
            let contact_url = format!("{}{}", home, href);
            match self
                .client
                .get(&contact_url)
                .basic_auth(username, Some(password))
                .send()
                .await
            {
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.ok();
                    if status.is_success() && body.as_ref().map(|t| !t.is_empty()).unwrap_or(false)
                    {
                        contacts.push(Contact {
                            href,
                            etag: Some(etag),
                            vcard: body.unwrap(),
                        });
                    }
                }
                _ => continue,
            }
        }

        Ok((contacts, new_sync_token))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_base_url_removes_creds() {
        let input = "http://user:pass@example.com/dav/card/";
        let out = CarddavClient::sanitize_base_url(input);
        assert!(!out.contains("@"));
        assert!(out.starts_with("http://example.com/"));
    }

    #[test]
    fn derive_base_from_caldav_maps_dav_root_to_dav_card() {
        assert_eq!(
            CarddavClient::derive_base_from_caldav("http://stalwart:8080/dav"),
            "http://stalwart:8080/dav/card"
        );
        assert_eq!(
            CarddavClient::derive_base_from_caldav("http://stalwart:8080/dav/"),
            "http://stalwart:8080/dav/card"
        );
    }

    #[test]
    fn derive_base_from_caldav_strips_calendar_subpath() {
        assert_eq!(
            CarddavClient::derive_base_from_caldav("http://stalwart:8080/dav/cal"),
            "http://stalwart:8080/dav/card"
        );
    }

    #[test]
    fn derive_base_from_caldav_ignores_host_containing_dav() {
        // A host literally named `dav…` must not be mistaken for the DAV root.
        assert_eq!(
            CarddavClient::derive_base_from_caldav("https://dav.example.com/caldav"),
            "https://dav.example.com/caldav/card"
        );
        assert_eq!(
            CarddavClient::derive_base_from_caldav("https://dav.example.com/dav"),
            "https://dav.example.com/dav/card"
        );
    }

    #[test]
    fn from_caldav_base_sets_dav_card_root() {
        let client = CarddavClient::from_caldav_base("http://stalwart:8080/dav/");
        assert_eq!(client.base, "http://stalwart:8080/dav/card");
    }

    #[test]
    fn addressbook_home_matches_stalwart_layout() {
        let client = CarddavClient {
            base: "http://stalwart:8080/dav/card".to_string(),
            client: Client::new(),
        };
        assert_eq!(
            client.addressbook_home("testuser@redacted.com"),
            "http://stalwart:8080/dav/card/testuser@redacted.com/"
        );
    }
}
