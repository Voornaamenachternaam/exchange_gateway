// src/directory.rs
// Directory service for GAL/ResolveNames functionality.
// Provides a trait-based abstraction for contact/recipient lookups.
// Implements HTTP-based directory lookup using Stalwart's admin API.

use chrono::{DateTime, Utc};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

/// A contact entry returned by directory lookups.
use serde_json::json;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Display name (e.g., "John Doe")
    pub display_name: String,
    /// Primary email address (SMTP)
    pub email: String,
    /// Optional title (e.g., "Senior Engineer")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Optional office location
    #[serde(skip_serializing_if = "Option::is_none")]
    pub office: Option<String>,
    /// Optional phone number
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Optional department
    #[serde(skip_serializing_if = "Option::is_none")]
    pub department: Option<String>,
    /// Optional company
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    /// When this entry was last updated (if known)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<DateTime<Utc>>,
}

/// A distribution list entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DistributionList {
    /// Display name of the distribution list
    pub display_name: String,
    /// Email address of the DL
    pub email: String,
    /// Number of members (may be approximate)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_count: Option<u32>,
    /// Whether this is a dynamic DL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_dynamic: Option<bool>,
}

/// Result of a directory search.
#[derive(Debug, Clone)]
pub struct SearchResult {
    /// Matching contacts
    pub contacts: Vec<Contact>,
    /// Matching distribution lists (if queried)
    pub distribution_lists: Vec<DistributionList>,
    /// Whether the result set is truncated (pagination limit reached)
    pub is_truncated: bool,
    /// Total count estimate (may be exact or lower bound)
    pub total_estimate: usize,
}

/// Errors that can occur during directory operations.
#[derive(Error, Debug)]
pub enum DirectoryError {
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Authentication failed")]
    AuthError,
    #[error("Search query too complex or invalid")]
    InvalidQuery,
    #[error("Operation timeout")]
    Timeout,
    #[error("Internal error: {0}")]
    Internal(String),
    #[error("HTTP error: {0}")]
    HttpError(String),
}

/// Trait for directory lookup services.
/// All methods are synchronous (blocking) and must be called from
/// tokio::task::spawn_blocking or similar to avoid blocking the async runtime.
pub trait DirectoryLookup: Send + Sync {
    /// Search for contacts by partial name or email.
    /// Returns up to `limit` results, or all if limit is None.
    fn search_blocking(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError>;

    /// Resolve a single email address to a contact.
    /// Returns None if not found.
    fn resolve_email_blocking(&self, email: &str) -> Result<Option<Contact>, DirectoryError>;

    /// Expand a distribution list to its members.
    fn expand_dl_blocking(&self, email: &str) -> Result<Vec<Contact>, DirectoryError>;

    /// Check if the directory service is available.
    fn is_available(&self) -> bool;
}

/// Async extension trait providing non-blocking wrappers.
#[async_trait::async_trait]
pub trait DirectoryLookupAsync: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError>;
    async fn resolve_email(&self, email: &str) -> Result<Option<Contact>, DirectoryError>;
    async fn expand_dl(&self, email: &str) -> Result<Vec<Contact>, DirectoryError>;
}

#[async_trait::async_trait]
impl<T: DirectoryLookup + Clone + Send + Sync + 'static> DirectoryLookupAsync for T {
    async fn search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError> {
        let this = self.clone();
        let query = query.to_string();
        tokio::task::spawn_blocking(move || this.search_blocking(&query, limit))
            .await
            .map_err(|e| DirectoryError::Internal(format!("Task join error: {}", e)))?
    }

    async fn resolve_email(&self, email: &str) -> Result<Option<Contact>, DirectoryError> {
        let this = self.clone();
        let email = email.to_string();
        tokio::task::spawn_blocking(move || this.resolve_email_blocking(&email))
            .await
            .map_err(|e| DirectoryError::Internal(format!("Task join error: {}", e)))?
    }

    async fn expand_dl(&self, email: &str) -> Result<Vec<Contact>, DirectoryError> {
        let this = self.clone();
        let email = email.to_string();
        tokio::task::spawn_blocking(move || this.expand_dl_blocking(&email))
            .await
            .map_err(|e| DirectoryError::Internal(format!("Task join error: {}", e)))?
    }
}

/// Configuration for Stalwart admin API directory.
#[derive(Debug, Clone)]
pub struct StalwartAdminConfig {
    /// Base URL for Stalwart admin API (e.g., "http://stalwart:8080/api/v1")
    pub base_url: String,
    /// Username for admin authentication (if required)
    pub username: Option<String>,
    /// Password for admin authentication (if required)
    pub password: Option<String>,
    /// Connection timeout in seconds
    pub timeout_secs: u64,
}

impl Default for StalwartAdminConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            username: None,
            password: None,
            timeout_secs: 10,
        }
    }
}

/// JMAP‑based directory service using Stalwart's Identity API.
/// Retrieves contacts via JMAP `Identity/get`.
use crate::jmap::JmapClient;
use secrecy::SecretString;
use tokio::runtime::Runtime;

pub struct JmapDirectory {
    jmap_client: JmapClient,
    username: String,
    password: SecretString,
    runtime: Runtime,
}

impl DirectoryLookup for JmapDirectory {
    fn search_blocking(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError> {
        // Use the pre‑created Tokio runtime stored in the struct.
        let rt = &self.runtime;
        let username = self.username.clone();
        let password = self.password.clone();
        let client = self.jmap_client.clone();
        let limit_val = limit.unwrap_or(100).min(200);
        rt.block_on(async move {
            // Account/get returns all accounts; we filter client‑side for the search query.
            let args = json!({});
            let resp = client
                .api_call(
                    &client.base_url(),
                    &["urn:ietf:params:jmap:core"],
                    vec![("Account/get", args, "a0")],
                    &username,
                    &password,
                )
                .await?;
            let mut contacts = Vec::new();
            if let Some((_, value, _)) = resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "Account/get")
            {
                if let Some(list) = value.get("list").and_then(|v| v.as_array()) {
                    for acc in list {
                        let email = acc
                            .get("email")
                            .or_else(|| acc.get("primaryEmail"))
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let display_name = acc
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&email)
                            .to_string();
                        if query.is_empty()
                            || display_name.to_lowercase().contains(&query.to_lowercase())
                            || email.to_lowercase().contains(&query.to_lowercase())
                        {
                            contacts.push(Contact {
                                display_name,
                                email,
                                title: None,
                                office: None,
                                phone: None,
                                department: None,
                                company: None,
                                last_modified: None,
                            });
                        }
                    }
                }
            }
            Ok(SearchResult {
                contacts,
                distribution_lists: vec![],
                is_truncated: false,
                total_estimate: contacts.len(),
            })
        })
    }

    fn resolve_email_blocking(&self, email: &str) -> Result<Option<Contact>, DirectoryError> {
        if !email.contains('@') {
            return Ok(None);
        }
        let rt = Runtime::new()
            .map_err(|e| DirectoryError::Internal(format!("Runtime create error: {}", e)))?;
        let username = self.username.clone();
        let password = self.password.clone();
        let client = self.jmap_client.clone();
        let email_str = email.to_string();
        rt.block_on(async move {
            // Query for the specific email.
            let query_args = json!({
                "filter": { "email": email_str },
                "limit": 1,
            });
            let query_resp = client
                .api_call(
                    &client.base_url(),
                    &["urn:ietf:params:jmap:core"],
                    vec![("Identity/query", query_args, "q0")],
                    &username,
                    &password,
                )
                .await?;
            let ids: Vec<String> = if let Some((_, value, _)) = query_resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "Identity/query")
            {
                value
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map_or(vec![], |arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_string()))
                            .collect()
                    })
            } else {
                vec![]
            };
            if ids.is_empty() {
                return Ok(None);
            }
            let get_args = json!({ "ids": ids });
            let get_resp = client
                .api_call(
                    &client.base_url(),
                    &["urn:ietf:params:jmap:core"],
                    vec![("Identity/get", get_args, "g0")],
                    &username,
                    &password,
                )
                .await?;
            if let Some((_, value, _)) = get_resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "Identity/get")
            {
                if let Some(list) = value.get("list").and_then(|v| v.as_array()) {
                    if let Some(item) = list.first() {
                        let email = item
                            .get("email")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();
                        let display_name = item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&email)
                            .to_string();
                        return Ok(Some(Contact {
                            display_name,
                            email,
                            title: None,
                            office: None,
                            phone: None,
                            department: None,
                            company: None,
                            last_modified: None,
                        }));
                    }
                }
            }
            Ok(None)
        })
    }

    fn expand_dl_blocking(&self, _email: &str) -> Result<Vec<Contact>, DirectoryError> {
        // Stalwart JMAP does not expose distribution lists.
        Ok(Vec::new())
    }

    fn is_available(&self) -> bool {
        !self.username.is_empty()
    }
}

#[allow(clippy::new_ret_no_self)]
impl StalwartAdminDirectory {
    pub fn new(config: StalwartAdminConfig) -> Result<Arc<dyn DirectoryLookup>, DirectoryError> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| {
                DirectoryError::NetworkError(format!("Failed to build HTTP client: {}", e))
            })?;

        Ok(Arc::new(Self { config, client }) as Arc<dyn DirectoryLookup>)
    }

    /// Build a query URL for searching accounts.
    fn build_search_url(&self, query: &str, limit: usize) -> String {
        let query_encoded = urlencoding::encode(query);
        format!(
            "{}/accounts?query={}&limit={}",
            self.config.base_url, query_encoded, limit
        )
    }

    /// Build a URL for getting a specific account by email.
    fn build_email_url(&self, email: &str) -> String {
        format!(
            "{}/accounts/{}",
            self.config.base_url,
            urlencoding::encode(email)
        )
    }

    /// Parse admin API response into SearchResult.
    fn parse_search_response(&self, json: &serde_json::Value) -> SearchResult {
        let mut contacts = Vec::new();

        if let Some(accounts) = json.get("accounts").and_then(|v| v.as_array()) {
            for item in accounts {
                let email = item
                    .get("username")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let display_name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                let title = item.get("title").and_then(|v| v.as_str()).map(String::from);
                let department = item
                    .get("department")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let company = item
                    .get("company")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let phone = item.get("phone").and_then(|v| v.as_str()).map(String::from);

                let last_modified = item
                    .get("modified")
                    .and_then(|v| v.as_str())
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));

                contacts.push(Contact {
                    display_name,
                    email,
                    title,
                    office: None,
                    phone,
                    department,
                    company,
                    last_modified,
                });
            }
        }

        let total_estimate = json
            .get("total")
            .and_then(|v| v.as_u64())
            .unwrap_or(contacts.len() as u64) as usize;
        let is_truncated = total_estimate > contacts.len();

        SearchResult {
            contacts,
            distribution_lists: Vec::new(),
            is_truncated,
            total_estimate,
        }
    }

    /// Parse a single account response into Contact.
    fn parse_email_response(&self, json: &serde_json::Value) -> Option<Contact> {
        let email = json.get("username")?.as_str()?.to_string();
        let display_name = json.get("name")?.as_str()?.to_string();

        let title = json.get("title").and_then(|v| v.as_str()).map(String::from);
        let department = json
            .get("department")
            .and_then(|v| v.as_str())
            .map(String::from);
        let company = json
            .get("company")
            .and_then(|v| v.as_str())
            .map(String::from);
        let phone = json.get("phone").and_then(|v| v.as_str()).map(String::from);

        let last_modified = json
            .get("modified")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&Utc));

        Some(Contact {
            display_name,
            email,
            title,
            office: None,
            phone,
            department,
            company,
            last_modified,
        })
    }
}

impl DirectoryLookup for StalwartAdminDirectory {
    fn search_blocking(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError> {
        if query.is_empty() {
            return Err(DirectoryError::InvalidQuery);
        }
        let limit_val = limit.unwrap_or(100).min(200);

        let url = self.build_search_url(query, limit_val);
        let req = self.client.get(&url);
        let req = match (&self.config.username, &self.config.password) {
            (Some(user), Some(pass)) => req.basic_auth(user, Some(pass)),
            _ => req,
        };

        let resp = req.send().map_err(|e| {
            if e.is_status() {
                DirectoryError::HttpError(format!("HTTP error: {}", e.status().unwrap()))
            } else if e.is_timeout() {
                DirectoryError::Timeout
            } else {
                DirectoryError::NetworkError(format!("Request failed: {}", e))
            }
        })?;

        if !resp.status().is_success() {
            return Err(DirectoryError::HttpError(format!(
                "Stalwart admin API returned {}",
                resp.status()
            )));
        }

        let json: serde_json::Value = resp.json().map_err(|e| {
            DirectoryError::HttpError(format!("Failed to parse JSON response: {}", e))
        })?;

        Ok(self.parse_search_response(&json))
    }

    fn resolve_email_blocking(&self, email: &str) -> Result<Option<Contact>, DirectoryError> {
        if !email.contains('@') {
            return Ok(None);
        }

        let url = self.build_email_url(email);
        let req = self.client.get(&url);
        let req = match (&self.config.username, &self.config.password) {
            (Some(user), Some(pass)) => req.basic_auth(user, Some(pass)),
            _ => req,
        };

        let resp = req.send().map_err(|e| {
            if e.is_status() {
                DirectoryError::HttpError(format!("HTTP error: {}", e.status().unwrap()))
            } else if e.is_timeout() {
                DirectoryError::Timeout
            } else {
                DirectoryError::NetworkError(format!("Request failed: {}", e))
            }
        })?;

        if resp.status() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            return Err(DirectoryError::HttpError(format!(
                "Stalwart admin API returned {}",
                resp.status()
            )));
        }

        let json: serde_json::Value = resp.json().map_err(|e| {
            DirectoryError::HttpError(format!("Failed to parse JSON response: {}", e))
        })?;

        Ok(self.parse_email_response(&json))
    }

    fn expand_dl_blocking(&self, _email: &str) -> Result<Vec<Contact>, DirectoryError> {
        // Distribution list expansion requires additional Stalwart API endpoints.
        // Not implemented in this initial version.
        Ok(Vec::new())
    }

    fn is_available(&self) -> bool {
        !self.config.base_url.is_empty()
    }
}

/// Null directory that returns empty results.
#[derive(Debug, Clone)]
pub struct NullDirectory;

impl DirectoryLookup for NullDirectory {
    fn search_blocking(
        &self,
        _query: &str,
        _limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError> {
        Ok(SearchResult {
            contacts: Vec::new(),
            distribution_lists: Vec::new(),
            is_truncated: false,
            total_estimate: 0,
        })
    }

    fn resolve_email_blocking(&self, _email: &str) -> Result<Option<Contact>, DirectoryError> {
        Ok(None)
    }

    fn expand_dl_blocking(&self, _email: &str) -> Result<Vec<Contact>, DirectoryError> {
        Ok(Vec::new())
    }

    fn is_available(&self) -> bool {
        false
    }
}

/// Create a directory client based on configuration.
/// Returns Arc<dyn DirectoryLookup>.
pub fn create_directory(
    admin_base: Option<&str>,
    admin_username: Option<&str>,
    admin_password: Option<&str>,
) -> Arc<dyn DirectoryLookup> {
    // Prefer the legacy admin API if a base URL is provided.
    if let Some(base) = admin_base.filter(|s| !s.is_empty()) {
        let config = StalwartAdminConfig {
            base_url: base.to_string(),
            username: admin_username.map(|u| u.to_string()),
            password: admin_password.map(|p| p.to_string()),
            timeout_secs: 10,
        };
        return match StalwartAdminDirectory::new(config) {
            Ok(dir) => dir,
            Err(_) => Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>,
        };
    }
    // Fallback to JMAP Identity lookup using the configured JMAP base URL.
    // Read JMAP base from environment (same variable used by Config).
    if let Ok(jmap_base) = std::env::var("GATEWAY_JMAP_BASE") {
        if !jmap_base.is_empty() {
            // Build JMAP client.
            match JmapClient::new(&jmap_base) {
                Ok(jc) => {
                    let username = admin_username.unwrap_or("").to_string();
                    let password = admin_password.unwrap_or("");
                    let dir = JmapDirectory {
                        jmap_client: jc,
                        username,
                        password: SecretString::from(password.to_string()),
                        runtime: Runtime::new()
                            .expect("Failed to create Tokio runtime for JmapDirectory"),
                    };
                    return Arc::new(dir) as Arc<dyn DirectoryLookup>;
                }
                Err(e) => {
                    tracing::warn!(target: "directory", error = %e, "Failed to create JMAP client for directory service");
                    return Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>;
                }
            }
        }
    }
    // No directory service configured.
    Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_directory() {
        let dir = NullDirectory;
        assert!(!dir.is_available());
        let res = dir.search_blocking("test", None).unwrap();
        assert!(res.contacts.is_empty());
    }
}
