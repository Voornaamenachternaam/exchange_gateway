// src/carddav.rs
use crate::config::Config;
use anyhow::{Result, anyhow};
use reqwest::Client;
use std::time::Duration;
use tracing::warn;

/// CardDAV client for contacts sync.
/// Supports Stalwart Mailserver v0.16.10 CardDAV endpoint at /dav/{user}/contacts/.
pub struct CarddavClient {
    base: String,
    client: Client,
}

/// Represents a contact entry from CardDAV.
#[derive(Debug, Clone)]
pub struct Contact {
    pub href: String,
    pub etag: String,
    pub vcard: String,
}

impl CarddavClient {
    /// Construct a new CardDAV client from configuration.
    pub fn new(cfg: &Config) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let base = Self::sanitize_base_url(&cfg.carddav_base);
        Ok(Self { base, client })
    }

    /// Derive CardDAV base from CalDAV base if not explicitly configured.
    /// Default: replace "/dav/" with "/carddav/" in caldav_base.
    pub fn from_caldav_base(caldav_base: &str) -> Self {
        let base = caldav_base
            .replace("/dav/", "/carddav/")
            .trim_end_matches('/')
            .to_string();
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("Failed to build CardDAV client");
        Self { base, client }
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

    /// Build the addressbook home set URL for a given user.
    /// Typically: {base}/carddav/{username}/
    pub fn addressbook_home(&self, username: &str) -> String {
        format!("{}/carddav/{}/", self.base.trim_end_matches('/'), username)
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
            .request(reqwest::Method::from_bytes(b"PROPFIND").unwrap_or(reqwest::Method::GET), &home_url)
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

    /// List all contacts in the default addressbook.
    /// Returns the raw XML multistatus response body.
    pub async fn list_contacts(&self, username: &str, password: &str) -> Result<String> {
        let home_url = self.addressbook_home(username);
        let body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getetag/>
    <D:getcontenttype/>
    <D:displayname/>
  </D:prop>
</D:propfind>"#;
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home_url)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml; charset=utf-8")
            .body(body)
            .send()
            .await?;
        if !resp.status().is_success() && resp.status().as_u16() != 207 {
            return Err(anyhow!("CardDAV PROPFIND failed: {}", resp.status()));
        }
        Ok(resp.text().await?)
    }

    /// Fetch a single contact vCard by its href (relative to addressbook home).
    pub async fn get_contact(&self, username: &str, password: &str, href: &str) -> Result<(String, Option<String>)> {
        let home = self.addressbook_home(username);
        let url = format!("{}{}", home, href);
        let resp = self.client.get(&url)
            .basic_auth(username, Some(password))
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(anyhow!("CardDAV GET failed: {}", resp.status()));
        }
        let etag = resp.headers().get("ETag")
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
            let loc_url = loc.to_str().map_err(|_| anyhow!("Invalid Location header"))?;
            // Get the part after the home URL
            loc_url.strip_prefix(&home).unwrap_or(loc_url).to_string()
        } else {
            // If no Location, use provided href or empty string
            href.unwrap_or("").to_string()
        };
        let etag = resp.headers().get("ETag")
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

    /// List contacts from the addressbook.
    /// Returns (contacts vec, sync_token). sync_token can be used for later sync queries.
    pub async fn list_contacts(
        &self,
        username: &str,
        password: &str,
        sync_token: Option<&str>,
    ) -> Result<(Vec<Contact>, Option<String>)> {
        let home = self.addressbook_home(username);
        // PROPFIND body to request vCard data and getetags
        let body = r#"<?xml version="1.0"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getetag/>
    <D:getcontenttype/>
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
        let doc = roxmltree::Document::parse(&xml).map_err(|e| anyhow!("XML parse error: {}", e))?;

        let mut contacts = Vec::new();
        let mut new_sync_token = None;

        // Process <D:response> elements
        for resp_elem in doc.descendants().filter(|n| n.has_tag_name("response")) {
            let href_elem = resp_elem.descendants().find(|n| n.has_tag_name("href"));
            let etag_elem = resp_elem.descendants().find(|n| n.has_tag_name("getetag"));
            let status_elem = resp_elem.descendants().find(|n| n.has_tag_name("status"));

            // Only process successful responses with a href and etag
            if let (Some(href_node), Some(etag_node)) = (href_elem, etag_elem) {
                let href = href_node.text().unwrap_or("").trim().to_string();
                let etag = etag_node.text().unwrap_or("").trim().to_string();

                // Skip the collection itself (href refers to addressbook collection, not item)
                if !href.is_empty() && etag != "\"\"" && !href.ends_with('/') {
                    // Fetch the vCard data for this contact
                    let contact_url = format!("{}{}", home, href);
                    let vcard_resp = self
                        .client
                        .get(&contact_url)
                        .basic_auth(username, Some(password))
                        .send()
                        .await?;
                    if vcard_resp.status().is_success() {
                        let vcard_text = vcard_resp.text().await?;
                        contacts.push(Contact {
                            href,
                            etag,
                            vcard: vcard_text,
                        });
                    }
                }
            }

            // If we find <D:sync-token> in the <D:propstat> of the collection href (empty href)
            // we capture it for future sync queries.
            if new_sync_token.is_none() {
                if let Some(etag_elem) = resp_elem.descendants().find(|n| n.has_tag_name("sync-token")) {
                    if let Some(tok) = etag_elem.text() {
                        new_sync_token = Some(tok.trim().to_string());
                    }
                }
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
        let input = "http://user:pass@example.com/carddav/";
        let out = CarddavClient::sanitize_base_url(input);
        assert!(!out.contains("@"));
        assert!(out.starts_with("http://example.com/"));
    }
}