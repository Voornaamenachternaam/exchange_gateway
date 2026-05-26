// src/jmap.rs
//
// JMAP (JSON Meta Application Protocol) client for email operations
// with Stalwart Mailserver v0.16.5.
//
// JMAP (RFC 8621) provides efficient email query/get/sync operations
// via a single HTTP endpoint. Stalwart v0.16.5 supports JMAP natively
// at the /jmap/ path with Basic authentication.
//
// This module implements:
// - Email query (search/list emails in a mailbox)
// - Email get (fetch full email content)
// - Email sync (delta updates via state tokens)
// - Mailbox query/get (list email folders)
//
// The gateway uses JMAP for email reading/syncing while keeping
// CalDAV for calendar operations. SMTP is used for email sending.

use anyhow::{anyhow, Result};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

/// JMAP session object (RFC 8621 §2.1)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapSession {
    pub api_url: String,
    pub download_url: String,
    pub upload_url: String,
    pub event_source_url: String,
    pub state: String,
    pub username: String,
    pub accounts: HashMap<String, JmapAccount>,
    pub primary_accounts: HashMap<String, String>,
    pub capabilities: HashMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapAccount {
    pub name: String,
    pub is_personal: bool,
    pub is_read_only: bool,
    pub account_capabilities: HashMap<String, Value>,
}

/// JMAP API request (RFC 8621 §3.1)
#[derive(Clone, Debug, Serialize)]
pub struct JmapRequest {
    pub using: Vec<String>,
    pub method_calls: Vec<JmapMethodCall>,
}

#[derive(Clone, Debug, Serialize)]
pub struct JmapMethodCall {
    pub name: String,
    pub arguments: Value,
    pub id: String,
}

/// JMAP API response
#[derive(Clone, Debug, Deserialize)]
pub struct JmapResponse {
    pub method_responses: Vec<(String, Value, String)>,
    #[serde(default)]
    #[allow(dead_code)]
    pub session_state: Option<String>,
}

/// JMAP Email object (RFC 8621 §4.1)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapEmail {
    pub id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub mailbox_ids: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub keywords: Option<HashMap<String, bool>>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub received_at: Option<String>,
    #[serde(default)]
    pub sent_at: Option<String>,
    #[serde(default)]
    pub has_attachment: Option<bool>,
    #[serde(default)]
    pub from: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub cc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub bcc: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub reply_to: Option<Vec<JmapEmailAddress>>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub preview: Option<String>,
    #[serde(default)]
    pub body_values: Option<HashMap<String, JmapBodyValue>>,
    #[serde(default)]
    pub text_body: Option<Vec<JmapBodyPart>>,
    #[serde(default)]
    pub html_body: Option<Vec<JmapBodyPart>>,
    #[serde(default)]
    pub attachments: Option<Vec<JmapAttachment>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapEmailAddress {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapBodyValue {
    pub value: String,
    #[serde(default)]
    pub is_encoding_problem: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapBodyPart {
    pub part_id: String,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub charset: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapAttachment {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub blob_id: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(rename = "type")]
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// JMAP Mailbox object (RFC 8621 §5.1)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JmapMailbox {
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub sort_order: Option<u64>,
    #[serde(default)]
    pub total_emails: Option<u64>,
    #[serde(default)]
    pub unread_emails: Option<u64>,
    #[serde(default)]
    pub total_threads: Option<u64>,
    #[serde(default)]
    pub unread_threads: Option<u64>,
    #[serde(default)]
    pub is_subscribed: Option<bool>,
}

/// Result of an Email/query call
#[derive(Clone, Debug)]
pub struct EmailListResult {
    pub emails: Vec<JmapEmail>,
    pub total: u64,
    pub can_calculate_changes: bool,
    pub query_state: String,
}

/// Result of an Email/changes call
#[derive(Clone, Debug)]
pub struct EmailChangesResult {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub new_state: String,
    pub has_more_changes: bool,
}

/// Result of a Mailbox/query call
#[derive(Clone, Debug)]
pub struct MailboxListResult {
    pub mailboxes: Vec<JmapMailbox>,
    pub total: u64,
}

/// Parameters for querying emails via JMAP.
pub struct QueryEmailsParams<'a> {
    pub account_id: &'a str,
    pub filter: Option<Value>,
    pub sort: Option<Vec<Value>>,
    pub position: u64,
    pub limit: u64,
    pub username: &'a str,
    pub password: &'a SecretString,
}

/// JMAP client for email operations via Stalwart Mailserver.
#[derive(Clone)]
pub struct JmapClient {
    base_url: String,
    client: reqwest::Client,
}

impl JmapClient {
    /// Create a new JMAP client pointing at the Stalwart JMAP endpoint.
    pub fn new(base_url: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| anyhow!("Failed to create JMAP HTTP client: {}", e))?;

        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            client,
        })
    }

    /// Derive the JMAP base URL from the CalDAV base URL.
    ///
    /// Stalwart serves JMAP at the same host as CalDAV, replacing
    /// the /dav/ path with /jmap/. For example:
    ///   http://stalwart:8080/dav → http://stalwart:8080/jmap
    pub fn derive_from_caldav(caldav_base: &str) -> String {
        let url = caldav_base.trim_end_matches('/');
        // Replace /dav suffix with /jmap
        if let Some(idx) = url.rfind("/dav") {
            format!("{}/jmap", &url[..idx])
        } else {
            // Fallback: append /jmap
            format!("{}/jmap", url)
        }
    }

    /// Build the Basic Authorization header value.
    fn basic_auth_header(username: &str, password: &SecretString) -> String {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(format!("{}:{}", username, password.expose_secret()));
        format!("Basic {}", encoded)
    }

    /// Fetch the JMAP session object (RFC 8621 §2.1).
    ///
    /// The session provides the API URL, account IDs, and capabilities.
    pub async fn get_session(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<JmapSession> {
        let url = format!("{}/session", self.base_url);
        let auth = Self::basic_auth_header(username, password);

        trace!(target: "jmap", url = %url, "Fetching JMAP session");

        let resp = self
            .client
            .get(&url)
            .header(AUTHORIZATION, &auth)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP session request failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP session request returned {}: {}", status, body));
        }

        resp.json::<JmapSession>()
            .await
            .map_err(|e| anyhow!("Failed to parse JMAP session: {}", e))
    }

    /// Make a JMAP API call (RFC 8621 §3.2).
    async fn api_call(
        &self,
        api_url: &str,
        using: &[&str],
        method_calls: Vec<(&str, Value, &str)>,
        username: &str,
        password: &SecretString,
    ) -> Result<JmapResponse> {
        let auth = Self::basic_auth_header(username, password);

        let calls: Vec<JmapMethodCall> = method_calls
            .into_iter()
            .map(|(name, arguments, id)| JmapMethodCall {
                name: name.to_string(),
                arguments,
                id: id.to_string(),
            })
            .collect();

        let request = JmapRequest {
            using: using.iter().map(|s| s.to_string()).collect(),
            method_calls: calls,
        };

        let body = serde_json::to_string(&request)
            .map_err(|e| anyhow!("Failed to serialize JMAP request: {}", e))?;

        trace!(target: "jmap", api_url = %api_url, body = %body, "JMAP API call");

        let resp = self
            .client
            .post(api_url)
            .header(AUTHORIZATION, &auth)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| anyhow!("JMAP API call failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("JMAP API call returned {}: {}", status, body));
        }

        resp.json::<JmapResponse>()
            .await
            .map_err(|e| anyhow!("Failed to parse JMAP response: {}", e))
    }

    /// Query emails in a mailbox.
    ///
    /// Maps to `Email/query` (RFC 8621 §4.3).
    pub async fn query_emails(&self, params: QueryEmailsParams<'_>) -> Result<EmailListResult> {
        let session = self.get_session(params.username, params.password).await?;
        let api_url = &session.api_url;

        let filter_val = params.filter.unwrap_or_else(|| json!({}));
        let sort_val = params.sort.unwrap_or_else(|| {
            vec![json!({"property": "receivedAt", "isAscending": false})]
        });

        let method_calls = vec![(
            "Email/query",
            json!({
                "accountId": params.account_id,
                "filter": filter_val,
                "sort": sort_val,
                "position": params.position,
                "limit": params.limit,
                "calculateTotal": true,
            }),
            "q0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                params.username,
                params.password,
            )
            .await?;

        // Parse the query response
        for (method, data, _) in response.method_responses {
            if method == "Email/query" {
                let ids: Vec<String> = data
                    .get("ids")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let total: u64 = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
                let can_calc: bool = data
                    .get("canCalculateChanges")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let query_state: String = data
                    .get("queryState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                // Fetch the actual email objects for the IDs
                let emails = if !ids.is_empty() {
                    self.get_emails(
                        params.account_id,
                        &ids,
                        Some(json!([
                            "id", "blobId", "threadId", "mailboxIds", "keywords",
                            "size", "receivedAt", "sentAt", "hasAttachment",
                            "from", "to", "cc", "bcc", "replyTo",
                            "subject", "preview", "bodyValues", "textBody", "htmlBody",
                            "attachments"
                        ])),
                        params.username,
                        params.password,
                    )
                    .await?
                } else {
                    Vec::new()
                };

                return Ok(EmailListResult {
                    emails,
                    total,
                    can_calculate_changes: can_calc,
                    query_state,
                });
            }
        }

        Err(anyhow!("Unexpected JMAP response structure for Email/query"))
    }

    /// Get specific emails by ID.
    ///
    /// Maps to `Email/get` (RFC 8621 §4.1).
    pub async fn get_emails(
        &self,
        account_id: &str,
        ids: &[String],
        properties: Option<Value>,
        username: &str,
        password: &SecretString,
    ) -> Result<Vec<JmapEmail>> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let props = properties.unwrap_or_else(|| {
            json!([
                "id", "blobId", "threadId", "mailboxIds", "keywords",
                "size", "receivedAt", "sentAt", "hasAttachment",
                "from", "to", "cc", "bcc", "replyTo",
                "subject", "preview"
            ])
        });

        let method_calls = vec![(
            "Email/get",
            json!({
                "accountId": account_id,
                "ids": ids,
                "properties": props,
                "bodyProperties": ["partId", "blobId", "size", "type", "charset"],
            }),
            "g0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/get" {
                let list: Vec<JmapEmail> = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                return Ok(list);
            }
        }

        Err(anyhow!("Unexpected JMAP response structure for Email/get"))
    }

    /// Get a single email by ID.
    pub async fn get_email(
        &self,
        account_id: &str,
        email_id: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<Option<JmapEmail>> {
        let emails = self
            .get_emails(
                account_id,
                &[email_id.to_string()],
                Some(json!([
                    "id", "blobId", "threadId", "mailboxIds", "keywords",
                    "size", "receivedAt", "sentAt", "hasAttachment",
                    "from", "to", "cc", "bcc", "replyTo",
                    "subject", "preview", "bodyValues", "textBody", "htmlBody",
                    "attachments"
                ])),
                username,
                password,
            )
            .await?;
        Ok(emails.into_iter().next())
    }

    /// Sync email changes since a given state token.
    ///
    /// Maps to `Email/changes` (RFC 8621 §4.4).
    pub async fn sync_email_changes(
        &self,
        account_id: &str,
        old_state: &str,
        username: &str,
        password: &SecretString,
    ) -> Result<EmailChangesResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        let method_calls = vec![(
            "Email/changes",
            json!({
                "accountId": account_id,
                "sinceState": old_state,
                "maxChanges": 500,
            }),
            "c0",
        )];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Email/changes" {
                let old_state = data
                    .get("oldState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let new_state = data
                    .get("newState")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let has_more: bool = data
                    .get("hasMoreChanges")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let created: Vec<String> = data
                    .get("created")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let updated: Vec<String> = data
                    .get("updated")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let destroyed: Vec<String> = data
                    .get("destroyed")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                debug!(
                    target: "jmap",
                    old_state = %old_state,
                    new_state = %new_state,
                    created = created.len(),
                    updated = updated.len(),
                    destroyed = destroyed.len(),
                    has_more = has_more,
                    "Email changes synced"
                );

                return Ok(EmailChangesResult {
                    created,
                    updated,
                    destroyed,
                    new_state,
                    has_more_changes: has_more,
                });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Email/changes"
        ))
    }

    
/// Update email properties via JMAP Email/set (RFC 8621 §4.5).
///
/// `update` is a JSON object mapping email IDs to patch objects.
/// Example: `{"email-123": {"keywords": {"$seen": true}}}`
pub async fn update_email(
    &self,
    account_id: &str,
    update: &serde_json::Value,
    username: &str,
    password: &SecretString,
) -> Result<()> {
    let session = self.get_session(username, password).await?;
    let api_url = &session.api_url;

    let method_calls = vec![(
        "Email/set",
        serde_json::json!({
            "accountId": account_id,
            "update": update,
        }),
        "es0",
    )];

    let _resp = self
        .api_call(
            api_url,
            &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            method_calls,
            username,
            password,
        )
        .await?;

    Ok(())
}

/// Destroy emails via JMAP Email/set (RFC 8621 §4.5).
///
/// `destroy` is a list of email IDs to permanently delete.
pub async fn destroy_emails(
    &self,
    account_id: &str,
    destroy: &[String],
    username: &str,
    password: &SecretString,
) -> Result<()> {
    let session = self.get_session(username, password).await?;
    let api_url = &session.api_url;

    let method_calls = vec![(
        "Email/set",
        serde_json::json!({
            "accountId": account_id,
            "destroy": destroy,
        }),
        "es0",
    )];

    let _resp = self
        .api_call(
            api_url,
            &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
            method_calls,
            username,
            password,
        )
        .await?;

    Ok(())
}
/// Query mailboxes for an account.
    ///
    /// Maps to `Mailbox/query` (RFC 8621 §5.3).
    pub async fn query_mailboxes(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<MailboxListResult> {
        let session = self.get_session(username, password).await?;
        let api_url = &session.api_url;

        // Find the primary account
        let account_id = session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))?;

        let method_calls = vec![
            (
                "Mailbox/query",
                json!({
                    "accountId": account_id,
                    "calculateTotal": true,
                }),
                "mq0",
            ),
            (
                "Mailbox/get",
                json!({
                    "accountId": account_id,
                    "#ids": {
                        "resultOf": "mq0",
                        "name": "Mailbox/query",
                        "path": "/ids",
                    },
                    "properties": [
                        "id", "name", "parentId", "role", "sortOrder",
                        "totalEmails", "unreadEmails", "totalThreads", "unreadThreads",
                        "isSubscribed"
                    ],
                }),
                "mg0",
            ),
        ];

        let response = self
            .api_call(
                api_url,
                &["urn:ietf:params:jmap:core", "urn:ietf:params:jmap:mail"],
                method_calls,
                username,
                password,
            )
            .await?;

        for (method, data, _) in response.method_responses {
            if method == "Mailbox/get" {
                let list: Vec<JmapMailbox> = data
                    .get("list")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();
                let total: u64 = list.len() as u64;
                return Ok(MailboxListResult { mailboxes: list, total });
            }
        }

        Err(anyhow!(
            "Unexpected JMAP response structure for Mailbox/get"
        ))
    }

    /// Get the primary mail account ID from the JMAP session.
    pub async fn get_account_id(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<String> {
        let session = self.get_session(username, password).await?;
        session
            .primary_accounts
            .get("urn:ietf:params:jmap:mail")
            .cloned()
            .ok_or_else(|| anyhow!("No primary mail account found in JMAP session"))
    }

    /// Verify JMAP credentials by fetching the session.
    pub async fn verify_credentials(
        &self,
        username: &str,
        password: &SecretString,
    ) -> Result<()> {
        self.get_session(username, password).await?;
        Ok(())
    }

    /// Health check: verify JMAP endpoint is reachable.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/session", self.base_url);
        let resp = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| anyhow!("JMAP health check failed: {}", e))?;

        match resp.status().as_u16() {
            200..=299 | 401 | 403 => {
                debug!(target: "jmap", url = %url, "JMAP health check passed");
                Ok(())
            }
            status => {
                warn!(target: "jmap", url = %url, status = status, "JMAP health check failed");
                Err(anyhow!("JMAP endpoint returned status {}", status))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn test_derive_from_caldav_basic() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/dav");
        assert_eq!(result, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_derive_from_caldav_with_trailing_slash() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/dav/");
        assert_eq!(result, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_derive_from_caldav_no_dav_suffix() {
        let result = JmapClient::derive_from_caldav("http://stalwart:8080/api");
        assert_eq!(result, "http://stalwart:8080/api/jmap");
    }

    #[test]
    fn test_jmap_client_new_strips_trailing_slash() {
        let client = JmapClient::new("http://stalwart:8080/jmap/").unwrap();
        assert_eq!(client.base_url, "http://stalwart:8080/jmap");
    }

    #[test]
    fn test_basic_auth_header_format() {
        let auth = JmapClient::basic_auth_header(
            "user@example.com",
            &SecretString::from("pass123"),
        );
        assert!(auth.starts_with("Basic "));
        // Base64 of "user@example.com:pass123"
        let expected_b64 = base64::engine::general_purpose::STANDARD
            .encode("user@example.com:pass123");
        assert_eq!(auth, format!("Basic {}", expected_b64));
    }
}