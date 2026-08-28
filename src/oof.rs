// src/oof.rs
// Out of Office (OOF) management using Stalwart's Sieve filtering.
// Maps EWS OOF settings to Sieve vacation scripts.

use chrono::Utc;
use secrecy::SecretString;
use tokio::runtime::Runtime;

use crate::jmap::{JMAP_SIEVE_CAPABILITY, JmapClient};
use serde_json::json;

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

/// OOF state as reported by GetUserOofSettings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OofSettings {
    /// Whether OOF is enabled
    pub enabled: bool,
    /// External audience setting
    pub external_audience: ExternalAudience,
    /// Internal reply message (if any)
    pub internal_reply: Option<String>,
    /// External reply message (if any)
    pub external_reply: Option<String>,
    /// Start time (UTC) for OOF, if scheduled
    pub start_time: Option<chrono::DateTime<Utc>>,
    /// End time (UTC) for OOF, if scheduled
    pub end_time: Option<chrono::DateTime<Utc>>,
}

/// Who receives external OOF replies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub enum ExternalAudience {
    /// Only external senders
    External,
    /// Known external senders (contacts)
    KnownExternal,
    /// All external senders
    #[default]
    All,
}

/// Errors that can occur during OOF operations.
#[derive(Error, Debug)]
pub enum OofError {
    #[error("Configuration error: {0}")]
    ConfigError(String),
    #[error("Network error: {0}")]
    NetworkError(String),
    #[error("Authentication failed")]
    AuthError,
    #[error("Invalid OOF state: {0}")]
    InvalidState(String),
    #[error("Sieve error: {0}")]
    SieveError(String),
    #[error("HTTP error: {0}")]
    HttpError(String),
    #[error("Timeout")]
    Timeout,
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Trait for OOF management service.
pub trait OofManager: Send + Sync {
    /// Get current OOF settings for a user.
    fn get_oof_settings(&self, username: &str) -> Result<OofSettings, OofError>;

    /// Set OOF settings for a user.
    fn set_oof_settings(
        &self,
        username: &str,
        settings: OofSettings,
    ) -> Result<OofSettings, OofError>;

    /// Check if OOF is currently active.
    fn is_oof_active(&self, username: &str) -> Result<bool, OofError> {
        let settings = self.get_oof_settings(username)?;
        if !settings.enabled {
            return Ok(false);
        }
        let now = Utc::now();
        let active = settings.start_time.is_none_or(|s| now >= s)
            && settings.end_time.is_none_or(|e| now <= e);
        Ok(active)
    }
}

/// Build the Sieve `vacation` script for the given OOF settings.
///
/// Pure helper shared by the JMAP OOF manager (and its unit tests): it maps
/// an `OofSettings`-derived audience/reply/date tuple onto a valid Sieve
/// script. Returns an empty string when both reply bodies are absent, which
/// disables the vacation action.
pub fn build_sieve_script(
    mail_domain: &str,
    internal_reply: Option<&str>,
    external_reply: Option<&str>,
    external_audience: ExternalAudience,
    start_time: Option<chrono::DateTime<Utc>>,
    end_time: Option<chrono::DateTime<Utc>>,
) -> Result<String, OofError> {
    // If neither reply is set, disable OOF.
    if internal_reply.is_none() && external_reply.is_none() {
        return Ok(String::new());
    }

    // Escape string for Sieve literal.
    let escape_sieve = |s: &str| -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '\\' => out.push_str("\\\\"),
                '"' => out.push_str("\\\""),
                '\n' => out.push(' '),
                '\r' => out.push(' '),
                _ => out.push(c),
            }
        }
        out
    };

    let internal_escaped = escape_sieve(internal_reply.unwrap_or(""));
    let external_escaped = escape_sieve(external_reply.unwrap_or(""));
    let domain_escaped = escape_sieve(mail_domain);

    // Build the list of required Sieve extensions.
    let mut requires = vec!["vacation", "envelope"];
    let date_required = start_time.is_some() && end_time.is_some();
    if date_required {
        // `currentdate :value` (RFC 5260) needs both the `date` extension (for
        // the `currentdate` test) and the `relational` extension (for the
        // `:value` match type).
        requires.push("date");
        requires.push("relational");
    }
    let require_str = requires
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    let mut script = format!("require [{}];\n", require_str);

    // Helper to create a condition block for a given audience.
    let audience_cond = |is_internal: bool| -> String {
        if is_internal {
            format!("envelope :domain \"from\" \"{}\"", domain_escaped)
        } else {
            format!("not envelope :domain \"from\" \"{}\"", domain_escaped)
        }
    };

    // Helper to create a vacation command with subject.
    let vacation_cmd = |body_escaped: &str| -> String {
        format!(
            "vacation :days 1 :subject \"Out of Office\" \"{}\";",
            body_escaped
        )
    };

    // Construct the rule blocks based on external_audience.
    let mut rule_blocks = String::new();
    match external_audience {
        ExternalAudience::All => {
            if !internal_escaped.is_empty() {
                rule_blocks.push_str(&format!(
                    "if allof ({}) {{\n  {}\n}}\n",
                    audience_cond(true),
                    vacation_cmd(&internal_escaped)
                ));
            }
            if !external_escaped.is_empty() {
                rule_blocks.push_str(&format!(
                    "if allof ({}) {{\n  {}\n}}\n",
                    audience_cond(false),
                    vacation_cmd(&external_escaped)
                ));
            }
        }
        ExternalAudience::External => {
            if !external_escaped.is_empty() {
                rule_blocks.push_str(&format!(
                    "if allof ({}) {{\n  {}\n}}\n",
                    audience_cond(false),
                    vacation_cmd(&external_escaped)
                ));
            }
        }
        ExternalAudience::KnownExternal => {
            // Treat KnownExternal same as External for now.
            if !external_escaped.is_empty() {
                rule_blocks.push_str(&format!(
                    "if allof ({}) {{\n  {}\n}}\n",
                    audience_cond(false),
                    vacation_cmd(&external_escaped)
                ));
            }
        }
    }

    // If no vacation action was produced (e.g. the audience never matched a
    // configured reply), return an empty script to disable OOF rather than
    // storing a `require`-only script that would report OOF as enabled without
    // ever sending a reply.
    if rule_blocks.trim().is_empty() {
        return Ok(String::new());
    }

    // If date range is provided, wrap the rule blocks in an outer `currentdate`
    // condition (RFC 5260): the test applies to the current date/time and needs
    // the `date` + `relational` capabilities and the `iso8601` date-part. The
    // timestamps are formatted as ISO 8601 (`YYYY-MM-DDTHH:MM:SS±ZZZZ`).
    if date_required {
        if let (Some(start), Some(end)) = (start_time, end_time) {
            let start_str = start.format("%Y-%m-%dT%H:%M:%S%z").to_string();
            let end_str = end.format("%Y-%m-%dT%H:%M:%S%z").to_string();
            let wrapped = format!(
                "if allof (currentdate :value \"ge\" \"iso8601\" \"{}\", currentdate :value \"le\" \"iso8601\" \"{}\") {{\n{}\n}}\n",
                start_str, end_str, rule_blocks
            );
            script.push_str(&wrapped);
        } else {
            script.push_str(&rule_blocks);
        }
    } else {
        script.push_str(&rule_blocks);
    }

    Ok(script)
}

/// JMAP‑based OOF manager using Stalwart's JMAP Sieve extension.
/// Stores vacation scripts via the `SieveScript` JMAP methods.
pub struct JmapOofManager {
    jmap_client: JmapClient,
    username: String,
    password: SecretString,
    mail_domain: String,
    runtime: std::sync::Arc<Runtime>,
}

// Implementation of JmapOofManager
impl JmapOofManager {
    /// Create a new JMAP OOF manager.
    pub fn create(
        jmap_base: &str,
        username: &str,
        password: &str,
        mail_domain: &str,
    ) -> Result<Arc<dyn OofManager>, OofError> {
        let client =
            JmapClient::new(jmap_base).map_err(|e| OofError::NetworkError(e.to_string()))?;
        Ok(Arc::new(Self {
            jmap_client: client,
            username: username.to_string(),
            password: SecretString::from(password.to_string()),
            mail_domain: mail_domain.to_string(),
            runtime: std::sync::Arc::new(
                Runtime::new().map_err(|e| OofError::Internal(e.to_string()))?,
            ),
        }) as Arc<dyn OofManager>)
    }

    fn get_script_blocking(&self, username: &str) -> Result<Option<String>, OofError> {
        let rt = self.runtime.clone();
        let client = self.jmap_client.clone();
        let usr = self.username.clone();
        let pwd = self.password.clone();
        let uname = username.to_string();
        rt.block_on(async move {
            let account_id = client
                .get_account_id(&usr, &pwd)
                .await
                .map_err(|e| OofError::NetworkError(e.to_string()))?;
            let args = json!({"accountId": account_id, "ids": [uname]});
            let resp = client
                .api_call(
                    client.base_url(),
                    &[JMAP_SIEVE_CAPABILITY],
                    vec![("SieveScript/get", args, "a0")],
                    &usr,
                    &pwd,
                )
                .await
                .map_err(|e| OofError::NetworkError(e.to_string()))?;
            if let Some((_name, value, _id)) = resp.method_responses.first()
                && let Some(list) = value.get("list").and_then(|v| v.as_array())
                && let Some(entry) = list.first()
                && let Some(script) = entry.get("script").and_then(|s| s.as_str())
            {
                return Ok(Some(script.to_string()));
            }
            Ok(None)
        })
    }

    fn set_script_blocking(&self, username: &str, script: &str) -> Result<(), OofError> {
        let rt = self.runtime.clone();
        let client = self.jmap_client.clone();
        let usr = self.username.clone();
        let pwd = self.password.clone();
        let uname = username.to_string();
        rt.block_on(async move {
            let account_id = client
                .get_account_id(&usr, &pwd)
                .await
                .map_err(|e| OofError::NetworkError(e.to_string()))?;
            let mut create_map = serde_json::Map::new();
            create_map.insert(uname.clone(), json!({"script": script}));
            let args = json!({
                "accountId": account_id,
                "update": create_map
            });
            client
                .api_call(
                    client.base_url(),
                    &[JMAP_SIEVE_CAPABILITY],
                    vec![("SieveScript/set", args, "a0")],
                    &usr,
                    &pwd,
                )
                .await
                .map_err(|e| OofError::NetworkError(e.to_string()))?;
            Ok(())
        })
    }
}

impl OofManager for JmapOofManager {
    fn get_oof_settings(&self, username: &str) -> Result<OofSettings, OofError> {
        let script_opt = self.get_script_blocking(username)?;
        let enabled = script_opt.as_ref().is_some_and(|s| s.contains("vacation"));
        Ok(OofSettings {
            enabled,
            external_audience: ExternalAudience::All,
            internal_reply: None,
            external_reply: None,
            start_time: None,
            end_time: None,
        })
    }

    fn set_oof_settings(
        &self,
        username: &str,
        settings: OofSettings,
    ) -> Result<OofSettings, OofError> {
        if !settings.enabled {
            self.set_script_blocking(username, "")?;
            return Ok(settings);
        }
        let script = build_sieve_script(
            &self.mail_domain,
            settings.internal_reply.as_deref(),
            settings.external_reply.as_deref(),
            settings.external_audience,
            settings.start_time,
            settings.end_time,
        )?;
        self.set_script_blocking(username, &script)?;
        Ok(settings)
    }

    fn is_oof_active(&self, username: &str) -> Result<bool, OofError> {
        let settings = self.get_oof_settings(username)?;
        if !settings.enabled {
            return Ok(false);
        }
        let now = Utc::now();
        let active = settings.start_time.is_none_or(|s| now >= s)
            && settings.end_time.is_none_or(|e| now <= e);
        Ok(active)
    }
}

/// Null OOF manager that always reports disabled.
#[derive(Debug, Clone)]
pub struct NullOofManager;

impl OofManager for NullOofManager {
    fn get_oof_settings(&self, _username: &str) -> Result<OofSettings, OofError> {
        Ok(OofSettings {
            enabled: false,
            external_audience: ExternalAudience::All,
            internal_reply: None,
            external_reply: None,
            start_time: None,
            end_time: None,
        })
    }

    fn set_oof_settings(
        &self,
        _username: &str,
        settings: OofSettings,
    ) -> Result<OofSettings, OofError> {
        Ok(settings)
    }

    fn is_oof_active(&self, _username: &str) -> Result<bool, OofError> {
        Ok(false)
    }
}

/// Create an OOF manager based on configuration.
///
/// OOF management is backed exclusively by Stalwart's JMAP Sieve extension
/// (`urn:ietf:params:jmap:sieve` `SieveScript/get`/`set`, RFC 9666): `jmap_base`
/// is the Stalwart JMAP endpoint and `admin_username`/`admin_password` are the
/// Stalwart account credentials whose Sieve scripts are managed. The deprecated
/// REST admin API (`/api/.../sieve`) is no longer used.
///
/// Returns a `JmapOofManager` when a non-blank `jmap_base` *and* both admin
/// credentials are present; otherwise a `NullOofManager` (OOF always reported
/// disabled). Blank credentials are treated the same as a missing endpoint so
/// that a partially-configured manager never issues requests that would only
/// fail at runtime.
pub fn create_oof_manager(
    jmap_base: Option<&str>,
    admin_username: Option<&str>,
    admin_password: Option<&str>,
    mail_domain: &str,
) -> Arc<dyn OofManager> {
    let Some(jmap_base) = jmap_base.filter(|s| !s.trim().is_empty()) else {
        return Arc::new(NullOofManager) as Arc<dyn OofManager>;
    };
    let (Some(user), Some(pass)) = (
        admin_username.filter(|s| !s.trim().is_empty()),
        admin_password.filter(|s| !s.is_empty()),
    ) else {
        tracing::warn!(
            target: "oof",
            "JMAP admin credentials are not configured; OOF will be reported disabled"
        );
        return Arc::new(NullOofManager) as Arc<dyn OofManager>;
    };
    match JmapOofManager::create(jmap_base, user, pass, mail_domain) {
        Ok(manager) => manager,
        Err(e) => {
            tracing::warn!(
                target: "oof",
                error = %e,
                "Failed to create JMAP OOF manager; OOF will be reported disabled"
            );
            Arc::new(NullOofManager) as Arc<dyn OofManager>
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_null_oof_manager() {
        let manager = NullOofManager;
        let settings = manager.get_oof_settings("user@example.com").unwrap();
        assert!(!settings.enabled);
        assert!(!manager.is_oof_active("user@example.com").unwrap());
    }

    #[test]
    fn test_build_sieve_script_enabled() {
        let internal_reply = Some("Internal");
        let external_reply = Some("External");
        let start_time = Some(Utc::now());
        let end_time = Some(Utc::now() + Duration::days(7));
        let script = build_sieve_script(
            "example.com",
            internal_reply,
            external_reply,
            ExternalAudience::All,
            start_time,
            end_time,
        )
        .unwrap();
        assert!(script.contains("vacation"));
        assert!(script.contains("example.com"));
    }

    #[test]
    fn test_build_sieve_script_disabled() {
        let internal_reply = None;
        let external_reply = None;
        let start_time = None;
        let end_time = None;
        let script = build_sieve_script(
            "example.com",
            internal_reply,
            external_reply,
            ExternalAudience::All,
            start_time,
            end_time,
        )
        .unwrap();
        assert!(!script.contains("vacation"));
        assert!(script.is_empty());
    }

    #[test]
    fn test_build_sieve_script_audience_mismatch_returns_empty() {
        // `External` audience with only an internal reply produces no vacation
        // action; the script must be empty (disable OOF), not a `require`-only
        // script that would falsely report OOF as enabled.
        let script = build_sieve_script(
            "example.com",
            Some("Internal only"),
            None,
            ExternalAudience::External,
            None,
            None,
        )
        .unwrap();
        assert!(script.is_empty());
    }

    #[test]
    fn test_build_sieve_script_date_range_uses_currentdate_and_relational() {
        let start = Utc::now();
        let end = start + Duration::days(1);
        let script = build_sieve_script(
            "example.com",
            Some("Internal"),
            Some("External"),
            ExternalAudience::All,
            Some(start),
            Some(end),
        )
        .unwrap();
        assert!(
            script.contains("\"relational\""),
            "relational capability required by :value"
        );
        assert!(
            script.contains("\"date\""),
            "date capability required by currentdate"
        );
        assert!(
            script.contains("currentdate :value \"ge\" \"iso8601\""),
            "lower bound uses currentdate"
        );
        assert!(
            script.contains("currentdate :value \"le\" \"iso8601\""),
            "upper bound uses currentdate"
        );
        // Timestamps must be ISO 8601 (`T` separator), not a space-separated form.
        assert!(script.contains('T'));
    }
}
