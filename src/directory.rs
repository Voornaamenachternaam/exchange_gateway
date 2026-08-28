// src/directory.rs
// Directory service for GAL/ResolveNames functionality.
//
// Provides a trait-based abstraction for contact/recipient lookups against the
// Stalwart back-end. The directory is read exclusively over JMAP using
// Stalwart's native admin extension (`urn:stalwart:jmap`):
//
//   * `x:Account/query` + `x:Account/get`  — enumerate/resolve user (and group)
//     accounts, i.e. the GAL / recipient directory.
//   * `x:MailingList/query` + `x:MailingList/get` — distribution lists.
//
// The deprecated Stalwart REST admin API (`/api/{v1,v2,...}/accounts`,
// `/api/.../sieve`, etc.) is NOT used anywhere in this module: the JMAP admin
// extension is the supported, forward-compatible surface for server-wide
// directory reads and requires the Stalwart administrator account that holds
// the `sysAccount*` / `sysMailingList*` permissions.

use chrono::{DateTime, Utc};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use thiserror::Error;
use tokio::runtime::Runtime;

use crate::jmap::{JMAP_STALWART_CAPABILITY, JmapClient, JmapResponse};

/// Upper bound on the number of accounts fetched in a single directory read.
/// The OAB download and the NSPI GAL snapshot already cap at 5000; this is a
/// safety ceiling so a misconfigured directory can never trigger an unbounded
/// full-directory transfer.
const MAX_ACCOUNTS: usize = 10_000;

/// A contact entry returned by directory lookups.
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

impl From<anyhow::Error> for DirectoryError {
    fn from(e: anyhow::Error) -> Self {
        DirectoryError::NetworkError(format!("Directory service request failed: {}", e))
    }
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

/// JMAP-backed directory service using Stalwart's admin JMAP extension.
///
/// The administrator account (which holds `sysAccountGet`/`sysAccountQuery` and
/// `sysMailingListGet`/`sysMailingListQuery` permissions) is used to enumerate
/// the server-wide directory via `x:Account/*` and `x:MailingList/*` under the
/// `urn:stalwart:jmap` capability.
pub struct JmapDirectory {
    jmap_client: JmapClient,
    username: String,
    password: SecretString,
    runtime: Runtime,
}

/// The account ID passed as the `accountId` argument to the `x:Account/*` and
/// `x:MailingList/*` `Foo/query` / `Foo/get` methods (RFC 8620 method shape).
/// Stalwart's admin (`sys*`) methods are scoped server-wide and do not operate
/// on a single mailbox; the admin extension accepts the reserved `admin`
/// account id for these calls.
const ADMIN_ACCOUNT_SCOPE: &str = "admin";

impl JmapDirectory {
    /// Construct a JMAP-backed directory service.
    pub fn create(
        jmap_base: &str,
        admin_username: Option<&str>,
        admin_password: Option<&str>,
    ) -> Result<Arc<dyn DirectoryLookup>, DirectoryError> {
        if jmap_base.trim().is_empty() {
            return Err(DirectoryError::ConfigError(
                "JMAP base URL is required for the directory service".to_string(),
            ));
        }
        let client = JmapClient::new(jmap_base).map_err(|e| {
            DirectoryError::ConfigError(format!("Failed to build JMAP client: {e}"))
        })?;
        let username = admin_username.unwrap_or("").to_string();
        let password = admin_password.unwrap_or("");
        let runtime = Runtime::new()
            .map_err(|e| DirectoryError::Internal(format!("Tokio runtime create failed: {e}")))?;
        Ok(Arc::new(Self {
            jmap_client: client,
            username,
            password: SecretString::from(password.to_string()),
            runtime,
        }) as Arc<dyn DirectoryLookup>)
    }

    /// Whether the directory is usable: a JMAP base and admin credentials are
    /// both required (the admin account is what holds the `sys*` permissions).
    fn configured(&self) -> bool {
        !self.username.is_empty() && !self.password.expose_secret().is_empty()
    }

    /// Perform a JMAP `x:Account/query` and return the account `ids`.
    fn query_account_ids(
        &self,
        filter: Value,
        limit: Option<usize>,
    ) -> Result<(Vec<String>, Option<usize>), DirectoryError> {
        let mut args = serde_json::Map::new();
        args.insert("accountId".to_string(), json!(ADMIN_ACCOUNT_SCOPE));
        args.insert("filter".to_string(), filter);
        if let Some(limit) = limit {
            args.insert("limit".to_string(), json!(limit));
            args.insert("position".to_string(), json!(0));
        }
        let rt = &self.runtime;
        let client = self.jmap_client.clone();
        let username = self.username.clone();
        let password = self.password.clone();
        rt.block_on(async move {
            let resp = client
                .api_call(
                    client.base_url(),
                    &["urn:ietf:params:jmap:core", JMAP_STALWART_CAPABILITY],
                    vec![("x:Account/query", Value::Object(args), "q0")],
                    &username,
                    &password,
                )
                .await?;
            if let Some(err) = jmap_method_error(&resp) {
                return Err(err);
            }
            let (ids, total) = if let Some((_, value, _)) = resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "x:Account/query")
            {
                let ids = value
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let total = value
                    .get("total")
                    .and_then(|v| v.as_u64())
                    .map(|n| n as usize);
                (ids, total)
            } else {
                (Vec::new(), None)
            };
            Ok((ids, total))
        })
    }

    /// Perform a JMAP `x:Account/get` for the given ids and return the raw
    /// account `list` objects.
    fn get_accounts(&self, ids: &[String]) -> Result<Vec<Value>, DirectoryError> {
        let args = json!({
            "accountId": ADMIN_ACCOUNT_SCOPE,
            "ids": ids,
            "properties": ["name", "emailAddress", "description", "type"],
        });
        let rt = &self.runtime;
        let client = self.jmap_client.clone();
        let username = self.username.clone();
        let password = self.password.clone();
        rt.block_on(async move {
            let resp = client
                .api_call(
                    client.base_url(),
                    &["urn:ietf:params:jmap:core", JMAP_STALWART_CAPABILITY],
                    vec![("x:Account/get", args, "g0")],
                    &username,
                    &password,
                )
                .await?;
            if let Some(err) = jmap_method_error(&resp) {
                return Err(err);
            }
            let list = if let Some((_, value, _)) = resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "x:Account/get")
            {
                value
                    .get("list")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            Ok(list)
        })
    }
}

impl DirectoryLookup for JmapDirectory {
    fn search_blocking(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<SearchResult, DirectoryError> {
        if !self.configured() {
            return Err(DirectoryError::ConfigError(
                "Directory service is not configured (admin credentials missing)".to_string(),
            ));
        }

        // `"*"` and the empty string both mean "match all" (the OAB download and
        // NSPI GAL snapshot use `"*"` to enumerate the whole directory). Any other
        // query is an ambiguous-name-resolve (ANR) substring match over the
        // display name or email address.
        let wildcard = query.is_empty() || query == "*";
        // `None` means "return all matching results" (DirectoryLookup contract),
        // so it maps to the safety ceiling rather than a small implicit default;
        // an explicit `limit` still caps the result and reports truncation.
        let limit_val = limit.unwrap_or(MAX_ACCOUNTS).min(MAX_ACCOUNTS);

        // Fetch the whole (bounded) account list and filter client-side. This
        // keeps substring semantics deterministic (independent of Stalwart's
        // `text` full-text tokenisation) and matches the prior behaviour of both
        // the JMAP and deprecated REST directory back-ends.
        let (ids, total) = self.query_account_ids(json!({}), Some(MAX_ACCOUNTS))?;
        let accounts = self.get_accounts(&ids)?;

        let mut contacts = Vec::new();
        for acc in &accounts {
            let Some(contact) = parse_account_contact(acc) else {
                continue;
            };
            if wildcard
                || contact
                    .display_name
                    .to_lowercase()
                    .contains(&query.to_lowercase())
                || contact.email.to_lowercase().contains(&query.to_lowercase())
            {
                contacts.push(contact);
            }
        }

        // The server-side total reflects the full matching set before our
        // `MAX_ACCOUNTS` ceiling; when it exceeds the ceiling the directory is
        // reporting a truncated view even before any client-side `limit` trim.
        let server_truncated = total.is_some_and(|t| t > MAX_ACCOUNTS);
        let client_truncated = contacts.len() > limit_val;
        let total_estimate = contacts.len();
        let is_truncated = server_truncated || client_truncated;
        if client_truncated {
            contacts.truncate(limit_val);
        }

        Ok(SearchResult {
            contacts,
            distribution_lists: Vec::new(),
            is_truncated,
            total_estimate,
        })
    }

    fn resolve_email_blocking(&self, email: &str) -> Result<Option<Contact>, DirectoryError> {
        if !self.configured() {
            return Err(DirectoryError::ConfigError(
                "Directory service is not configured (admin credentials missing)".to_string(),
            ));
        }
        if !email.contains('@') {
            return Ok(None);
        }

        // Exact-match on the email address. `x:Account/query` has no dedicated
        // `email` filter condition, so we filter by `text` and then confirm the
        // exact address client-side.
        let lower = email.to_lowercase();
        let (ids, _total) = self.query_account_ids(json!({ "text": email }), Some(MAX_ACCOUNTS))?;
        let accounts = self.get_accounts(&ids)?;
        for acc in &accounts {
            if let Some(contact) = parse_account_contact(acc)
                && contact.email.to_lowercase() == lower
            {
                return Ok(Some(contact));
            }
        }
        Ok(None)
    }

    fn expand_dl_blocking(&self, email: &str) -> Result<Vec<Contact>, DirectoryError> {
        if !self.configured() {
            return Err(DirectoryError::ConfigError(
                "Directory service is not configured (admin credentials missing)".to_string(),
            ));
        }
        if !email.contains('@') {
            return Ok(Vec::new());
        }

        let lower = email.to_lowercase();
        let rt = &self.runtime;
        let client = self.jmap_client.clone();
        let username = self.username.clone();
        let password = self.password.clone();
        let email_owned = email.to_string();

        // Resolve the mailing list by its address, then expand its members.
        rt.block_on(async move {
            let list_args = json!({
                "accountId": ADMIN_ACCOUNT_SCOPE,
                "filter": { "text": email_owned },
            });
            let list_resp = client
                .api_call(
                    client.base_url(),
                    &["urn:ietf:params:jmap:core", JMAP_STALWART_CAPABILITY],
                    vec![("x:MailingList/query", list_args, "q0")],
                    &username,
                    &password,
                )
                .await?;
            if let Some(err) = jmap_method_error(&list_resp) {
                return Err(err);
            }
            let list_ids: Vec<String> = if let Some((_, value, _)) = list_resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "x:MailingList/query")
            {
                value
                    .get("ids")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str().map(str::to_string))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            if list_ids.is_empty() {
                return Ok(Vec::new());
            }

            let get_args = json!({
                "accountId": ADMIN_ACCOUNT_SCOPE,
                "ids": list_ids,
                "properties": ["name", "emailAddress", "description", "recipients"],
            });
            let get_resp = client
                .api_call(
                    client.base_url(),
                    &["urn:ietf:params:jmap:core", JMAP_STALWART_CAPABILITY],
                    vec![("x:MailingList/get", get_args, "g0")],
                    &username,
                    &password,
                )
                .await?;
            if let Some(err) = jmap_method_error(&get_resp) {
                return Err(err);
            }

            let mut members = Vec::new();
            if let Some((_, value, _)) = get_resp
                .method_responses
                .iter()
                .find(|(name, _, _)| name == "x:MailingList/get")
                && let Some(list) = value.get("list").and_then(|v| v.as_array())
            {
                for entry in list {
                    // Only the mailing list whose address exactly matches the
                    // requested address is expanded. A missing/empty
                    // `emailAddress` never matches (so an unrelated list cannot
                    // be expanded for any requested address).
                    if !matches_list_address(entry, &lower) {
                        continue;
                    }
                    members = expand_recipients(entry);
                    break;
                }
            }
            Ok(members)
        })
    }

    fn is_available(&self) -> bool {
        self.configured()
    }
}

/// Inspect a parsed JMAP response for a method-level `error` response (RFC 8620
/// §3.6.2) and map it onto a `DirectoryError`.
///
/// JMAP reports per-method failures inside a successful HTTP 200 body as an
/// `error` entry (named `"error"` with a `{ type, description }` object), rather
/// than as an HTTP error. Without this check, a permission failure
/// (`forbidden`), a missing capability/unsupported method (`unknownMethod`), an
/// unknown object (`accountNotFound`), or `requestTooLarge` would otherwise be
/// indistinguishable from an empty (but valid) result set — silently returning
/// an empty GAL / no members instead of surfacing the failure.
///
/// Returns `None` when the response contains no `error` entry.
fn jmap_method_error(resp: &JmapResponse) -> Option<DirectoryError> {
    for (name, value, _) in &resp.method_responses {
        if name == "error" {
            let type_str = value
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let description = value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            return match type_str {
                "forbidden"
                | "accountNotFound"
                | "fromAccountNotFound"
                | "accountNotSupportedByMethod"
                | "invalidCredentials" => Some(DirectoryError::AuthError),
                "unknownMethod" | "serverUnavailable" | "serverFail" | "requestTooLarge" => {
                    Some(DirectoryError::HttpError(format!(
                        "JMAP method error: {type_str}: {description}"
                    )))
                }
                other => Some(DirectoryError::HttpError(format!(
                    "JMAP method error: {other}: {description}"
                ))),
            };
        }
    }
    None
}

/// Map a Stalwart `x:Account/get` account object (`@type: "User"` or `"Group"`)
/// to a directory `Contact`. Returns `None` for account variants that have no
/// resolvable email address (e.g. role/tenant objects).
fn parse_account_contact(acc: &Value) -> Option<Contact> {
    let name = acc.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let email = acc
        .get("emailAddress")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            // Fallback for servers that only populate `name` as the full address.
            if name.contains('@') {
                Some(name.to_string())
            } else {
                None
            }
        })?;

    let display_name = acc
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if name.is_empty() {
                email.clone()
            } else {
                name.to_string()
            }
        });

    Some(Contact {
        display_name,
        email,
        title: None,
        office: None,
        phone: None,
        department: None,
        company: None,
        last_modified: None,
    })
}

/// Whether a mailing-list object's `emailAddress` equals the requested (already
/// lower-cased) address. A missing or empty `emailAddress` never matches, which
/// prevents an unrelated list from being expanded for an arbitrary request.
fn matches_list_address(entry: &Value, requested_lower: &str) -> bool {
    entry
        .get("emailAddress")
        .and_then(|v| v.as_str())
        .is_some_and(|addr| addr.to_lowercase() == requested_lower)
}

/// Expand a mailing-list entry's `recipients` map into directory `Contact`s.
/// Stalwart models list members as a map of `recipientAddress -> {name?, ...}`;
/// each member becomes a contact whose address is the recipient key and whose
/// display name is the optional member name (falling back to the address).
fn expand_recipients(entry: &Value) -> Vec<Contact> {
    let Some(recipients) = entry.get("recipients").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    recipients
        .iter()
        .filter_map(|(address, member)| {
            if !address.contains('@') {
                return None;
            }
            let display_name = member
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| address.clone());
            Some(Contact {
                display_name,
                email: address.clone(),
                title: None,
                office: None,
                phone: None,
                department: None,
                company: None,
                last_modified: None,
            })
        })
        .collect()
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
///
/// The directory is backed exclusively by JMAP: `jmap_base` is the Stalwart JMAP
/// endpoint and `admin_username`/`admin_password` are the credentials of the
/// Stalwart administrator account holding the `sysAccount*`/`sysMailingList*`
/// permissions. The deprecated REST admin API is no longer used.
///
/// Returns `Arc<dyn DirectoryLookup>` (a `JmapDirectory` when configured,
/// otherwise a `NullDirectory`).
pub fn create_directory(
    jmap_base: Option<&str>,
    admin_username: Option<&str>,
    admin_password: Option<&str>,
) -> Arc<dyn DirectoryLookup> {
    let Some(base) = jmap_base.filter(|s| !s.trim().is_empty()) else {
        return Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>;
    };
    let Some(user) = admin_username.filter(|s| !s.is_empty()) else {
        tracing::warn!(
            target: "directory",
            "Directory JMAP base configured but no admin username; directory unavailable"
        );
        return Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>;
    };
    let pass = admin_password.unwrap_or("");
    if pass.is_empty() {
        tracing::warn!(
            target: "directory",
            "Directory JMAP base configured but no admin password; directory unavailable"
        );
        return Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>;
    }

    match JmapDirectory::create(base, Some(user), Some(pass)) {
        Ok(dir) => dir,
        Err(e) => {
            tracing::warn!(
                target: "directory",
                error = %e,
                "Failed to create JMAP directory service"
            );
            Arc::new(NullDirectory) as Arc<dyn DirectoryLookup>
        }
    }
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

    #[test]
    fn test_null_directory_resolve() {
        let dir = NullDirectory;
        assert!(
            dir.resolve_email_blocking("a@example.com")
                .unwrap()
                .is_none()
        );
        assert!(
            dir.expand_dl_blocking("list@example.com")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn test_parse_account_contact_uses_email_address_and_description() {
        let acc = json!({
            "id": "u1",
            "type": "User",
            "name": "alice",
            "emailAddress": "alice@example.com",
            "description": "Alice Example",
        });
        let contact = parse_account_contact(&acc).unwrap();
        assert_eq!(contact.email, "alice@example.com");
        assert_eq!(contact.display_name, "Alice Example");
    }

    #[test]
    fn test_parse_account_contact_falls_back_to_name() {
        let acc = json!({
            "id": "u2",
            "type": "User",
            "name": "bob",
            "emailAddress": "bob@example.com",
        });
        let contact = parse_account_contact(&acc).unwrap();
        assert_eq!(contact.email, "bob@example.com");
        assert_eq!(contact.display_name, "bob");
    }

    #[test]
    fn test_parse_account_contact_none_without_email() {
        let acc = json!({
            "id": "r1",
            "type": "Role",
            "description": "Role without an address",
        });
        assert!(parse_account_contact(&acc).is_none());
    }

    #[test]
    fn test_expand_recipients_maps_addresses_to_contacts() {
        let entry = json!({
            "name": "team",
            "emailAddress": "team@example.com",
            "recipients": {
                "alice@example.com": { "name": "Alice" },
                "bob@example.com": {}
            }
        });
        let members = expand_recipients(&entry);
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].email, "alice@example.com");
        assert_eq!(members[0].display_name, "Alice");
        assert_eq!(members[1].email, "bob@example.com");
        assert_eq!(members[1].display_name, "bob@example.com");
    }

    #[test]
    fn test_matches_list_address_exact_case_insensitive() {
        let entry = json!({ "emailAddress": "Team@Example.com" });
        assert!(matches_list_address(&entry, "team@example.com"));
    }

    #[test]
    fn test_matches_list_address_rejects_different_address() {
        let entry = json!({ "emailAddress": "other@example.com" });
        assert!(!matches_list_address(&entry, "team@example.com"));
    }

    #[test]
    fn test_matches_list_address_rejects_missing_email() {
        let entry = json!({ "name": "team" });
        assert!(!matches_list_address(&entry, "team@example.com"));
    }

    #[test]
    fn test_matches_list_address_rejects_empty_email() {
        let entry = json!({ "emailAddress": "" });
        assert!(!matches_list_address(&entry, "team@example.com"));
    }

    #[test]
    fn test_create_directory_unconfigured_returns_null() {
        let dir = create_directory(None, None, None);
        assert!(!dir.is_available());
        let dir = create_directory(Some("http://stalwart:8080/jmap"), None, None);
        assert!(!dir.is_available());
    }
}
