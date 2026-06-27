// src/oof.rs
// Out of Office (OOF) management using Stalwart's Sieve filtering.
// Maps EWS OOF settings to Sieve vacation scripts.

use chrono::Utc;
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ExternalAudience {
    /// Only external senders
    External,
    /// Known external senders (contacts)
    KnownExternal,
    /// All external senders
    All,
}

#[allow(clippy::derivable_impls)]
impl Default for ExternalAudience {
    fn default() -> Self {
        Self::All
    }
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
}

/// Trait for OOF management service.
pub trait OofManager: Send + Sync {
    /// Get current OOF settings for a user.
    fn get_oof_settings(&self, username: &str) -> Result<OofSettings, OofError>;
    
    /// Set OOF settings for a user.
    fn set_oof_settings(&self, username: &str, settings: OofSettings) -> Result<OofSettings, OofError>;
    
    /// Check if OOF is currently active.
    fn is_oof_active(&self, username: &str) -> Result<bool, OofError>;
}

/// Stalwart-based OOF manager using the admin API to manage Sieve scripts.
pub struct StalwartOofManager {
    admin_base: String,
    admin_username: Option<String>,
    admin_password: Option<String>,
    mail_domain: String,
    client: reqwest::blocking::Client,
}

impl StalwartOofManager {
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        admin_base: &str,
        admin_username: Option<&str>,
        admin_password: Option<&str>,
        mail_domain: &str,
    ) -> Result<Arc<dyn OofManager>, OofError> {
        if admin_base.is_empty() {
            return Err(OofError::ConfigError("Admin base URL is required".to_string()));
        }
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| OofError::NetworkError(format!("Failed to build HTTP client: {}", e)))?;
        Ok(Arc::new(Self {
            admin_base: admin_base.to_string(),
            admin_username: admin_username.map(String::from),
            admin_password: admin_password.map(String::from),
            mail_domain: mail_domain.to_string(),
            client,
        }) as Arc<dyn OofManager>)
    }
    
    /// Build the Sieve script for the given OOF settings.
fn build_sieve_script(
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
        requires.push("date");
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

    // If date range is provided, wrap the rule blocks in an outer date condition.
    if date_required {
        if let (Some(start), Some(end)) = (start_time, end_time) {
            // Convert to Sieve date format: "YYYY-MM-DD HH:MM:SS +ZZZZ"
            let start_str = start.format("%Y-%m-%d %H:%M:%S %z").to_string();
            let end_str = end.format("%Y-%m-%d %H:%M:%S %z").to_string();
            let wrapped = format!(
                "if allof (date :value \"ge\" \"{}\", date :value \"le\" \"{}\") {{\n{}\n}}\n",
                start_str,
                end_str,
                rule_blocks
            );
            script.push_str(&wrapped);
        } else {
            script.push_str(&rule_blocks);
        }
    } else {
        script.push_str(&rule_blocks);
    }

    // If the script contains no vacation actions, return empty to disable.
    if !script.contains("vacation") {
        Ok(String::new())
    } else {
        Ok(script)
    }
}
    
    fn get_current_script(&self, username: &str) -> Result<String, OofError> {
        let url = format!("{}/sieve/{}", self.admin_base, urlencoding::encode(username));
        let req = self.client.get(&url);
        let req = match (&self.admin_username, &self.admin_password) {
            (Some(u), Some(p)) => req.basic_auth(u, Some(p)),
            _ => req,
        };
        
        let resp = req.send().map_err(|e| {
            if e.is_timeout() { OofError::Timeout }
            else if e.is_status() { OofError::HttpError(format!("HTTP {}", e.status().unwrap())) }
            else { OofError::NetworkError(format!("{}", e)) }
        })?;
        
        if resp.status() == 404 {
            return Ok(String::new());
        }
        if !resp.status().is_success() {
            return Err(OofError::HttpError(format!("Stalwart returned {}", resp.status())));
        }
        
        resp.text().map_err(|e| OofError::HttpError(format!("Failed to read body: {}", e)))
    }
    
    fn set_script(&self, username: &str, script: &str) -> Result<(), OofError> {
        let url = format!("{}/sieve/{}", self.admin_base, urlencoding::encode(username));
        let mut req = self.client.put(&url);
        req = match (&self.admin_username, &self.admin_password) {
            (Some(u), Some(p)) => req.basic_auth(u, Some(p)),
            _ => req,
        };
        
        let resp = req.body(script.to_string())
            .header("Content-Type", "application/sieve")
            .send()
            .map_err(|e| {
                if e.is_timeout() { OofError::Timeout }
                else if e.is_status() { OofError::HttpError(format!("HTTP {}", e.status().unwrap())) }
                else { OofError::NetworkError(format!("{}", e)) }
            })?;
        
        if !resp.status().is_success() {
            return Err(OofError::HttpError(format!("Stalwart returned {}", resp.status())));
        }
        
        Ok(())
    }
}

impl OofManager for StalwartOofManager {
    fn get_oof_settings(&self, username: &str) -> Result<OofSettings, OofError> {
        match self.get_current_script(username) {
            Ok(script) => {
                let enabled = script.contains("vacation");
                if enabled {
                    // Parse duration and messages from script if needed
                    Ok(OofSettings {
                        enabled,
                        external_audience: ExternalAudience::All,
                        internal_reply: None,
                        external_reply: None,
                        start_time: None,
                        end_time: None,
                    })
                } else {
                    Ok(OofSettings {
                        enabled: false,
                        external_audience: ExternalAudience::All,
                        internal_reply: None,
                        external_reply: None,
                        start_time: None,
                        end_time: None,
                    })
                }
            }
            Err(e) => {
                if let OofError::HttpError(ref msg) = e && msg.contains("404") {
                    return Ok(OofSettings {
                        enabled: false,
                        external_audience: ExternalAudience::All,
                        internal_reply: None,
                        external_reply: None,
                        start_time: None,
                        end_time: None,
                    });
                }
                Err(e)
            }
        }
    }
    
    fn set_oof_settings(&self, username: &str, settings: OofSettings) -> Result<OofSettings, OofError> {
        if !settings.enabled {
            self.set_script(username, "")?;
            return Ok(settings);
        }
        
        let internal_reply = settings.internal_reply.as_deref();
        let external_reply = settings.external_reply.as_deref();
        let external_audience = settings.external_audience;
        let start_time = settings.start_time;
        let end_time = settings.end_time;
        
        let script = Self::build_sieve_script(
            &self.mail_domain,
            internal_reply,
            external_reply,
            external_audience,
            start_time,
            end_time,
        )?;
        self.set_script(username, &script)?;
        Ok(settings)
    }
    
    fn is_oof_active(&self, username: &str) -> Result<bool, OofError> {
        let settings = self.get_oof_settings(username)?;
        if !settings.enabled {
            return Ok(false);
        }
        let now = Utc::now();
        let active = settings.start_time.map(|start| now >= start).unwrap_or(true)
            && settings.end_time.map(|end| now <= end).unwrap_or(true);
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
    
    fn set_oof_settings(&self, _username: &str, settings: OofSettings) -> Result<OofSettings, OofError> {
        Ok(settings)
    }
    
    fn is_oof_active(&self, _username: &str) -> Result<bool, OofError> {
        Ok(false)
    }
}

/// Create an OOF manager based on configuration.
pub fn create_oof_manager(
    admin_base: Option<&str>,
    admin_username: Option<&str>,
    admin_password: Option<&str>,
    mail_domain: &str,
) -> Arc<dyn OofManager> {
    match (admin_base, admin_username, admin_password) {
        (Some(base), Some(user), Some(pass)) => {
            match StalwartOofManager::new(base, Some(user), Some(pass), mail_domain) {
                Ok(m) => m,
                Err(_) => Arc::new(NullOofManager) as Arc<dyn OofManager>,
            }
        }
        (Some(base), None, None) => {
            match StalwartOofManager::new(base, None, None, mail_domain) {
                Ok(m) => m,
                Err(_) => Arc::new(NullOofManager) as Arc<dyn OofManager>,
            }
        }
        _ => Arc::new(NullOofManager) as Arc<dyn OofManager>,
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
        let script = StalwartOofManager::build_sieve_script(
            "example.com",
            internal_reply,
            external_reply,
            ExternalAudience::All,
            start_time,
            end_time,
        ).unwrap();
        assert!(script.contains("vacation"));
        assert!(script.contains("example.com"));
    }
    
    #[test]
    fn test_build_sieve_script_disabled() {
        let internal_reply = None;
        let external_reply = None;
        let start_time = None;
        let end_time = None;
        let script = StalwartOofManager::build_sieve_script(
            "example.com",
            internal_reply,
            external_reply,
            ExternalAudience::All,
            start_time,
            end_time,
        ).unwrap();
        assert!(!script.contains("vacation"));
        assert!(script.is_empty());
    }
}