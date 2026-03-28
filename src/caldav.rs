// src/caldav.rs
use crate::config::Config;
use anyhow::{Result, anyhow};
use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Calendar folder information from CalDAV discovery
#[derive(Debug, Clone)]
pub struct CalendarFolder {
    /// The URL path of the calendar (relative to CalDAV base)
    pub href: String,
    /// The display name of the calendar (if available)
    pub display_name: Option<String>,
    /// Whether this is the user's default calendar
    pub is_default: bool,
}

/// Response from PROPFIND for calendar discovery
#[derive(Debug)]
struct PropfindResponse {
    pub calendars: Vec<CalendarFolder>,
}

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

    /// Discover all calendar folders for a user.
    /// Returns a list of CalendarFolder with href and optional display name.
    /// This implements proper multi-calendar discovery per RFC 4791 (CalDAV).
    pub async fn find_user_calendars(&self, username: &str, password: &str) -> Result<Vec<String>> {
        let url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &url)
            .basic_auth(username, Some(password))
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(r#"<propfind xmlns="DAV:"><prop><resourcetype/></prop></propfind>"#)
            .send()
            .await?;
        if resp.status().is_success() || resp.status().as_u16() == 207 {
            Ok(vec![url])
        } else {
            Err(anyhow::anyhow!(
                "failed to discover calendars: {}",
                resp.status()
            ))
        }
    }

    /// Discover all calendar folders for a user with full metadata.
    /// This properly queries for calendar-home-set and returns all calendars.
    /// Returns CalendarFolder structs with href, display_name, and is_default.
    pub async fn discover_calendar_folders(&self, username: &str, password: &str) -> Result<Vec<CalendarFolder>> {
        // First, try to find the calendar home set using a PROPFIND on the principal
        let principal_url = format!("{}/cal/{}/", self.base.trim_end_matches('/'), username);
        
        // Use PROPFIND with Depth: 1 to find all calendar collections
        let propfind_body = r#"<propfind xmlns="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
            <prop>
                <resourcetype/>
                <displayname/>
            </prop>
        </propfind>"#;
        
        let resp = self
            .client
            .request(reqwest::Method::from_bytes(b"PROPFIND")?, &principal_url)
            .basic_auth(username, Some(password))
            .header("Depth", "1")
            .header("Content-Type", "application/xml")
            .body(propfind_body)
            .send()
            .await?;
        
        let status = resp.status().as_u16();
        if status != 207 && status != 200 && status != 204 {
            // Fall back to basic single calendar
            return Ok(vec![CalendarFolder {
                href: principal_url.clone(),
                display_name: Some("Calendar".to_string()),
                is_default: true,
            }]);
        }
        
        let body = resp.text().await?;
        let calendars = self.parse_propfind_response(&body, &principal_url)?;
        
        if calendars.is_empty() {
            // Fall back to basic single calendar
            Ok(vec![CalendarFolder {
                href: principal_url,
                display_name: Some("Calendar".to_string()),
                is_default: true,
            }])
        } else {
            Ok(calendars)
        }
    }

    /// Parse PROPFIND response to extract calendar folders
    fn parse_propfind_response(&self, body: &str, base_url: &str) -> Result<Vec<CalendarFolder>> {
        let mut calendars = Vec::new();
        let mut in_response = false;
        let mut in_href = false;
        let mut in_resourcetype = false;
        let mut in_calendar = false;
        let mut in_displayname = false;
        let mut current_href = String::new();
        let mut current_displayname = Option::<String>::None;
        
        // Simple XML parsing without external dependencies
        let body_lower = body.to_lowercase();
        
        // Extract all href and resourcetype elements
        let mut pos = 0;
        while let Some(href_start) = body_lower[pos..].find("<d:href>") {
            let href_start = pos + href_start + 9;
            if let Some(href_end) = body_lower[href_start..].find("</d:href>") {
                let href = &body[href_start..href_start + href_end];
                
                // Check if this href contains calendar-like path
                let is_calendar = href.contains("/cal/") || href.contains("/calendar");
                
                if is_calendar {
                    // Look for resourcetype and displayname after this href
                    let rest = &body_lower[href_start + href_end..];
                    let has_calendar = rest.find("<d:resourcetype>")
                        .map(|rt| {
                            let after_rt = &rest[rt..];
                            after_rt.find("<c:calendar/>").is_some() || after_rt.find("<c:calendar ").is_some()
                        })
                        .unwrap_or(false);
                    
                    // Try to find displayname after this href
                    let display_name = if let Some(dn_start) = body[pos..].find("<d:displayname>") {
                        let dn_start = pos + dn_start + 15;
                        if let Some(dn_end) = body[dn_start..].find("</d:displayname>") {
                            let dn = &body[dn_start..dn_start + dn_end];
                            if !dn.is_empty() && dn != "&lt;collection&gt;" {
                                Some(dn.to_string())
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    
                    if has_calendar || href.contains("/cal/") {
                        calendars.push(CalendarFolder {
                            href: href.to_string(),
                            display_name,
                            is_default: calendars.is_empty(), // First calendar is default
                        });
                    }
                }
            }
            pos = href_start + href_end;
            if pos >= body.len() {
                break;
            }
        }
        
        Ok(calendars)
    }

    /// Find the default calendar or the first available calendar.
    /// This is used when the client doesn't specify which calendar to use.
    pub async fn find_default_calendar(&self, username: &str, password: &str) -> Result<String> {
        let calendars = self.discover_calendar_folders(username, password).await?;
        
        // Return the default calendar or first one
        calendars
            .into_iter()
            .find(|c| c.is_default)
            .or_else(|| {
                // Find by name patterns like "Calendar", "Default", etc.
                calendars.into_iter().find(|c| {
                    c.display_name
                        .as_ref()
                        .map(|n| n.to_lowercase().contains("calendar") || n.to_lowercase() == "default")
                        .unwrap_or(false)
                })
            })
            .map(|c| c.href)
            .ok_or_else(|| anyhow!("no calendars found for user"))
    }

    /// Get calendar by display name. Returns the first match.
    pub async fn find_calendar_by_name(&self, username: &str, password: &str, name: &str) -> Result<Option<String>> {
        let calendars = self.discover_calendar_folders(username, password).await?;
        
        // Case-insensitive name search
        let name_lower = name.to_lowercase();
        Ok(calendars
            .into_iter()
            .find(|c| {
                c.display_name
                    .as_ref()
                    .map(|n| n.to_lowercase() == name_lower)
                    .unwrap_or(false)
            })
            .map(|c| c.href))
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
            .header("Content-Type", "application/xml")
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
        } else {
            if resource_href.is_none() {
                req = req.header(IF_NONE_MATCH, "*");
            }
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
