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
    /// No external OOF replies at all ([MS-OXWOOF] §2.2.5.1 `None`, EAS
    /// `OofMessage` with every external audience disabled).
    None,
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
        // No external audience: only the internal rule (if any) applies.
        ExternalAudience::None => {
            if !internal_escaped.is_empty() {
                rule_blocks.push_str(&format!(
                    "if allof ({}) {{\n  {}\n}}\n",
                    audience_cond(true),
                    vacation_cmd(&internal_escaped)
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
            // Sieve cannot consult the owner's contacts, so "known senders"
            // cannot be distinguished from arbitrary external senders; replies
            // therefore go to every external sender (the same as `All`). The
            // alternative — refusing the setting — would make the client's
            // "known contacts only" choice fail outright, and suppressing
            // external replies entirely would silently ignore a configured
            // absence message.
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
    // bounds are UTC with a trailing `Z` (RFC 3339 §5.6 null offset), which every
    // `iso8601` parser reads as UTC; a bare `+0000` offset is only unambiguous in
    // ISO 8601, and without `:zone` a parser that ignores it would compare the
    // server's LOCAL wall time against these bounds.
    if date_required {
        if let (Some(start), Some(end)) = (start_time, end_time) {
            let start_str = start
                .with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
            let end_str = end
                .with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
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

/// Extract the human-readable OOF reply bodies from a Sieve script (RFC 5230).
///
/// The gateway's `build_sieve_script` emits one `vacation` command per audience,
/// keyed by the enclosing `if` condition — `envelope :domain "from" "<domain>"`
/// for internal senders and `not envelope :domain "from" "<domain>"` for
/// external senders. Both commands have the shape
/// `vacation :days 1 :subject "Out of Office" "<body>";`, where `<body>` is the
/// *last* double-quoted literal before the terminating `;`.
///
/// This helper locates each *real* `vacation` command (a bare identifier, not
/// the `"vacation"` capability string inside `require [ ... ]`), recovers its
/// body with Sieve escapes (`\\`, `\"`, `\n`) reversed, and returns
/// `(internal_reply, external_reply)`. A `None` on either side means that
/// audience has no reply configured (OOF disabled for it).
fn parse_vacation_replies(script: &str) -> (Option<String>, Option<String>) {
    let mut internal = None;
    let mut external = None;
    for cmd in vacation_commands(script) {
        let (is_internal, body) = cmd;
        if body.is_none() {
            continue;
        }
        if is_internal {
            if internal.is_none() {
                internal = body;
            }
        } else if external.is_none() {
            external = body;
        }
    }
    (internal, external)
}

/// Recover the scheduled-OOF window from the `currentdate` wrapper that
/// [`build_sieve_script`] emits (RFC 5260):
///
/// ```text
/// if allof (currentdate :value "ge" "iso8601" "2026-01-01T00:00:00Z",
///           currentdate :value "le" "iso8601" "2026-01-05T00:00:00Z") { ... }
///
/// Scripts stored before the `Z` form carry an explicit `+0000` offset; both
/// are read, because Stalwart keeps the script that was stored.
/// ```
///
/// Returns `(None, None)` when the script carries no window.
pub fn parse_vacation_window(
    script: &str,
) -> (Option<chrono::DateTime<Utc>>, Option<chrono::DateTime<Utc>>) {
    // Search ONLY the OOF section, which ends at the gateway-managed MAPI rules
    // block. Rule names are rendered there as `# rule <id>: <name>` comments and
    // a name may itself contain a `currentdate :value "ge" "iso8601" "<ts>"`
    // clause; scanning the whole script would then report a forged scheduled
    // window through `get_oof_settings` (delivery is unaffected — the text is a
    // Sieve comment — but the reported OofState/times would be wrong). The
    // gateway always appends the rules block AFTER the vacation script, so a
    // genuine window still parses.
    let oof_section: String = match script
        .lines()
        .position(|l| l.trim_end() == crate::mapi::rules::BLOCK_BEGIN)
    {
        Some(idx) => script.lines().take(idx).collect::<Vec<_>>().join("\n"),
        None => script.to_string(),
    };
    let script = oof_section.as_str();
    let date_after = |op: &str| -> Option<chrono::DateTime<Utc>> {
        let marker = format!("currentdate :value \"{op}\" \"iso8601\" \"");
        let rest = &script[script.find(&marker)? + marker.len()..];
        let value = &rest[..rest.find('"')?];
        // Current builds emit `...Z`; scripts stored before that carry an
        // explicit `+0000` offset, so both forms are read.
        chrono::DateTime::parse_from_rfc3339(value)
            .or_else(|_| chrono::DateTime::parse_from_str(value, "%Y-%m-%dT%H:%M:%S%z"))
            .ok()
            .map(|d| d.with_timezone(&Utc))
    };
    (date_after("ge"), date_after("le"))
}

/// Locate every real `vacation` command in a Sieve script, in document order,
/// each tagged `(is_internal, body)`. Skips the `"vacation"` capability string
/// inside `require [ ... ]` (where the keyword is a quoted literal, not a
/// command), and skips any `vacation` that appears inside a running string.
fn vacation_commands(script: &str) -> Vec<(bool, Option<String>)> {
    let bytes = script.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut in_string = false;

    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\\' {
            // Skip the escaped char so a `\"` inside a string is not seen as
            // the string terminator, and `\n` etc. stay opaque here.
            i += 2;
            continue;
        }
        if c == b'"' {
            in_string = !in_string;
            i += 1;
            continue;
        }
        if !in_string && bytes[i..].starts_with(b"vacation") {
            // A real command: the keyword is followed by whitespace or `:` (its
            // first `:days`/`:subject` tagged argument), not by a string quote.
            let next = bytes.get(i + b"vacation".len()).copied();
            if matches!(next, Some(b' ') | Some(b':') | Some(b'\t') | Some(b'\n')) {
                let (is_internal, body) = parse_vacation_command(script, i);
                out.push((is_internal, body));
            }
        }
        i += 1;
    }
    out
}

/// Parse the `vacation` command starting at byte offset `start` (which points at
/// the `v` of the `vacation` keyword). Returns `(is_internal, body)` where
/// `is_internal` comes from the enclosing `if` condition and `body` is the last
/// string literal before the command's terminating `;`.
fn parse_vacation_command(script: &str, start: usize) -> (bool, Option<String>) {
    let is_internal = is_internal_audience(script, start);
    let body = last_string_literal_before_terminator(script, start);
    (is_internal, body)
}

/// Return `true` when the `vacation` command at byte offset `start` sits inside
/// an internal-audience block (`envelope :domain "from"`, no `not`), and `false`
/// when it is external (`not envelope :domain "from"`). The gateway's own
/// scripts place the command directly inside its `if allof (...)` block, so the
/// nearest `envelope` before the command is the governing test; the token
/// immediately preceding that `envelope` is `not` exactly for the external block.
fn is_internal_audience(script: &str, start: usize) -> bool {
    let before = &script[..start];
    let Some(env) = before.rfind("envelope") else {
        return true;
    };
    // Inspect the token boundary just before `envelope`: the external condition
    // is `not envelope ...`, so a trailing `not` (preceded by a boundary) marks
    // the external audience.
    let prefix = before[..env].trim_end();
    !prefix.ends_with("not")
}

/// Extract the body of the `vacation` command at `start`: the last non-empty
/// string literal before the command's top-level `;`, with Sieve escapes
/// reversed. Returns `None` if the command carries no body string.
fn last_string_literal_before_terminator(script: &str, start: usize) -> Option<String> {
    let chars: Vec<char> = script[start..].chars().collect();
    let mut i = 0usize;
    let mut in_string = false;
    let mut string_literals: Vec<String> = Vec::new();
    let mut current = String::new();
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            if c == '\\' && i + 1 < chars.len() {
                match chars[i + 1] {
                    'n' => current.push('\n'),
                    '"' => current.push('"'),
                    '\\' => current.push('\\'),
                    other => current.push(other),
                }
                i += 2;
                continue;
            }
            if c == '"' {
                in_string = false;
                string_literals.push(std::mem::take(&mut current));
                i += 1;
                continue;
            }
            current.push(c);
            i += 1;
            continue;
        }
        match c {
            '"' => in_string = true,
            ';' => break,
            _ => {}
        }
        i += 1;
    }
    string_literals
        .into_iter()
        .rev()
        .find(|s| !s.trim().is_empty())
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
                    &["urn:ietf:params:jmap:core", JMAP_SIEVE_CAPABILITY],
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
                    &["urn:ietf:params:jmap:core", JMAP_SIEVE_CAPABILITY],
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
        // Surface the actual OOF reply text so `GetMailTips`/`GetUserOofSettings`
        // report the message the mailbox would send rather than an empty body.
        // The Sieve script keys replies by audience (`envelope :domain "from"`
        // vs `not envelope :domain "from"`), and this parser recovers each
        // audience's body independently rather than collapsing both to a single
        // reply. A script with no vacation action yields `None` for both.
        let (internal_reply, external_reply) = script_opt
            .as_deref()
            .map(parse_vacation_replies)
            .unwrap_or((None, None));
        // Recover a `Scheduled` window from the emitted `currentdate` wrapper so
        // GetUserOofSettings can report `Scheduled` with its real Duration
        // instead of collapsing every scheduled OOF into a plain `Enabled`.
        let (start_time, end_time) = script_opt
            .as_deref()
            .map(parse_vacation_window)
            .unwrap_or((None, None));
        Ok(OofSettings {
            enabled,
            external_audience: ExternalAudience::All,
            internal_reply,
            external_reply,
            start_time,
            end_time,
        })
    }

    fn set_oof_settings(
        &self,
        username: &str,
        settings: OofSettings,
    ) -> Result<OofSettings, OofError> {
        // The Sieve script is a shared resource: the MAPI rules engine
        // (`mapi::rules`) persists inbox rules in the same active script. Any
        // rewrite (enable or disable OOF) must carry the rules block across,
        // otherwise changing OOF would silently delete the user's rules.
        // Propagate a read failure rather than replacing what we could not
        // read.
        let carry_rules = self
            .get_script_blocking(username)?
            .and_then(|existing| crate::mapi::rules::rules_segment(&existing));
        let append_rules = |script: &mut String| {
            if let Some(block) = &carry_rules
                && !script.contains(block.as_str())
            {
                script.push('\n');
                script.push_str(block);
                script.push('\n');
            }
        };
        if !settings.enabled {
            let mut script = String::new();
            append_rules(&mut script);
            self.set_script_blocking(username, &script)?;
            return Ok(settings);
        }
        let mut script = build_sieve_script(
            &self.mail_domain,
            settings.internal_reply.as_deref(),
            settings.external_reply.as_deref(),
            settings.external_audience,
            settings.start_time,
            settings.end_time,
        )?;
        append_rules(&mut script);
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
    use chrono::{Duration, Timelike};

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

    /// A rule NAME rendered inside the MAPI rules block can contain a
    /// `currentdate :value …` clause. The window parser must stop at the block
    /// boundary, so a rule name can never fabricate a scheduled absence.
    #[test]
    fn test_parse_vacation_window_ignores_rule_names_in_the_rules_block() {
        let script = format!(
            "require [\"vacation\"];\n\
             vacation :days 1 :subject \"Away\" \"body\";\n\
             {begin}\n\
             # rule 1: currentdate :value \"ge\" \"iso8601\" \"2026-01-01T00:00:00+0000\" currentdate :value \"le\" \"iso8601\" \"2026-01-05T00:00:00+0000\"\n\
             {end}\n",
            begin = crate::mapi::rules::BLOCK_BEGIN,
            end = "# MAPI-RULES-END",
        );
        assert_eq!(
            parse_vacation_window(&script),
            (None, None),
            "a rule name must never be reported as a scheduled OOF window"
        );
    }

    /// A genuine scheduled window in the OOF section still parses when the rules
    /// block is appended after it.
    #[test]
    fn test_parse_vacation_window_still_parses_before_the_rules_block() {
        // The script carries whole seconds, so a sub-second `now()` would not
        // round-trip.
        let start = Utc::now()
            .with_nanosecond(0)
            .expect("nanosecond truncation");
        let end = start + Duration::days(2);
        let mut script = build_sieve_script(
            "example.com",
            Some("Internal"),
            Some("External"),
            ExternalAudience::All,
            Some(start),
            Some(end),
        )
        .unwrap();
        script.push('\n');
        script.push_str(crate::mapi::rules::BLOCK_BEGIN);
        script.push_str(
            "\n# rule 1: currentdate :value \"ge\" \"iso8601\" \"2001-01-01T00:00:00+0000\"\n",
        );
        script.push_str("# MAPI-RULES-END\n");
        let (parsed_start, parsed_end) = parse_vacation_window(&script);
        assert_eq!(
            parsed_start.map(|d| d.timestamp()),
            Some(start.timestamp()),
            "the OOF section's lower bound must still parse"
        );
        assert_eq!(
            parsed_end.map(|d| d.timestamp()),
            Some(end.timestamp()),
            "the OOF section's upper bound must still parse"
        );
    }

    /// The emitted bounds are UTC with a trailing `Z` (unambiguous for both
    /// RFC 3339 and ISO 8601 parsers), and a script stored in the older
    /// `+0000` form still parses to the same instant.
    #[test]
    fn test_vacation_window_bounds_are_utc_and_legacy_offsets_still_parse() {
        let start = chrono::DateTime::parse_from_rfc3339("2026-03-01T08:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let end = chrono::DateTime::parse_from_rfc3339("2026-03-03T17:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let script = build_sieve_script(
            "example.com",
            Some("Internal"),
            None,
            ExternalAudience::None,
            Some(start),
            Some(end),
        )
        .unwrap();
        assert!(
            script.contains("\"2026-03-01T08:30:00Z\"")
                && script.contains("\"2026-03-03T17:00:00Z\""),
            "bounds must be Z-suffixed UTC: {script}"
        );
        assert_eq!(parse_vacation_window(&script), (Some(start), Some(end)));

        let legacy = "if allof (currentdate :value \"ge\" \"iso8601\" \"2026-03-01T08:30:00+0000\",                       currentdate :value \"le\" \"iso8601\" \"2026-03-03T17:00:00+0000\") {\n}\n";
        assert_eq!(
            parse_vacation_window(legacy),
            (Some(start), Some(end)),
            "a previously stored +0000 window still parses"
        );
    }

    #[test]
    fn test_parse_vacation_replies_maps_audiences() {
        let script = build_sieve_script(
            "example.com",
            Some("Internal body"),
            Some("External body"),
            ExternalAudience::All,
            None,
            None,
        )
        .unwrap();
        let (internal, external) = parse_vacation_replies(&script);
        assert_eq!(internal.as_deref(), Some("Internal body"));
        assert_eq!(external.as_deref(), Some("External body"));
    }

    #[test]
    fn test_parse_vacation_replies_skips_require_capability() {
        // The `"vacation"` capability string inside `require [ ... ]` must not be
        // mistaken for a command; only the real command's body is recovered.
        let script = "require [\"vacation\", \"envelope\"];\nif allof (envelope :domain \"from\" \"example.com\") {\n  vacation :days 1 :subject \"Out of Office\" \"Only body\";\n}\n";
        let (internal, external) = parse_vacation_replies(script);
        assert_eq!(internal.as_deref(), Some("Only body"));
        assert_eq!(external, None);
    }

    #[test]
    fn test_parse_vacation_replies_external_only() {
        let script = build_sieve_script(
            "example.com",
            None,
            Some("External only"),
            ExternalAudience::External,
            None,
            None,
        )
        .unwrap();
        let (internal, external) = parse_vacation_replies(&script);
        assert_eq!(internal, None);
        assert_eq!(external.as_deref(), Some("External only"));
    }

    #[test]
    fn test_parse_vacation_replies_unescapes_sieve() {
        // `escape_sieve` turns `"` into `\"` (and folds newlines to spaces); the
        // parser must reverse the `\"` escape so the body round-trips.
        let script = build_sieve_script(
            "example.com",
            Some("Say \"hi\" now"),
            None,
            ExternalAudience::All,
            None,
            None,
        )
        .unwrap();
        let (internal, _external) = parse_vacation_replies(&script);
        assert_eq!(internal.as_deref(), Some("Say \"hi\" now"));
    }

    #[test]
    fn test_parse_vacation_replies_no_vacation_returns_none() {
        assert_eq!(
            parse_vacation_replies("require [\"envelope\"];\n"),
            (None, None)
        );
        assert_eq!(parse_vacation_replies(""), (None, None));
    }
}
