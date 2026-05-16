// src/auth.rs
use crate::caldav::CaldavClient;
use crate::config::Config;
use moka::sync::Cache;
use std::time::Duration;
use tracing::{debug, trace, warn};

/// Result of credential verification against the CalDAV backend.
pub enum CaldavAuthResult {
    /// Credentials confirmed valid (2xx or 207 response).
    Valid,
    /// Credentials confirmed invalid (401/403 or other auth failure).
    Invalid,
    /// Could not reach the CalDAV server (connection error, timeout, DNS failure).
    Unreachable,
}

#[derive(Clone)]
pub struct AuthVerifier {
    cache: Cache<String, bool>,
    caldav_base: String,
}

impl AuthVerifier {
    pub fn new(cfg: &Config) -> Self {
        let ttl = Duration::from_secs(cfg.auth_cache_ttl_secs.max(30));
        let max_entries = cfg.auth_cache_max_entries.max(1);
        let cache = Cache::builder()
            .time_to_live(ttl)
            .max_capacity(max_entries as u64)
            .build();
        Self {
            cache,
            caldav_base: cfg.caldav_base.clone(),
        }
    }

    pub async fn verify(&self, username: &str, password: &str) -> bool {
        if username.is_empty() || password.is_empty() {
            debug!(
                target: "auth",
                username = ?username,
                "Rejected empty credentials"
            );
            return false;
        }
        let cache_key = format!("{}:{}", username, hash_password_fast(password));
        if let Some(valid) = self.cache.get(&cache_key) {
            trace!(
                target: "auth",
                username = %username,
                cache_hit = true,
                valid = valid,
                "Authentication cache lookup"
            );
            return valid;
        }
        trace!(
            target: "auth",
            username = %username,
            "Cache miss - verifying with CalDAV"
        );
        let caldav = match CaldavClient::new_from_base(&self.caldav_base) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    target: "auth",
                    username = %username,
                    error = %e,
                    "Failed to create CalDAV client for auth - treating as unreachable"
                );
                // Fail-open: if we can't even create a client, the backend is down.
                // Don't cache this result; let previously-authenticated users through.
                return self.fail_open_or_reject(username);
            }
        };
        match caldav.verify_credentials_detailed(username, password).await {
            CaldavAuthResult::Valid => {
                self.cache.insert(cache_key, true);
                debug!(
                    target: "auth",
                    username = %username,
                    valid = true,
                    "Authentication succeeded and cached"
                );
                true
            }
            CaldavAuthResult::Invalid => {
                self.cache.insert(cache_key, false);
                debug!(
                    target: "auth",
                    username = %username,
                    valid = false,
                    "Authentication failed and cached"
                );
                false
            }
            CaldavAuthResult::Unreachable => {
                // Backend is down — don't poison the cache with a negative entry.
                // Let previously-authenticated users through (fail-open).
                warn!(
                    target: "auth",
                    username = %username,
                    "CalDAV backend unreachable during auth verification; fail-open"
                );
                self.fail_open_or_reject(username)
            }
        }
    }

    /// When the CalDAV backend is unreachable, we fail-open for users who have
    /// a recent successful auth cached under any password hash, and fail-closed
    /// for users with no prior successful auth (likely genuinely unknown).
    fn fail_open_or_reject(&self, username: &str) -> bool {
        // Check if we've ever seen a successful auth for this username.
        // Moka doesn't support prefix scans, so we use a secondary "known user" flag.
        let known_key = format!("known:{}", username);
        if self.cache.get(&known_key).is_some() {
            debug!(
                target: "auth",
                username = %username,
                "Backend unreachable, but user previously authenticated — allowing (fail-open)"
            );
            true
        } else {
            debug!(
                target: "auth",
                username = %username,
                "Backend unreachable and no prior successful auth — rejecting (fail-closed)"
            );
            false
        }
    }

    /// Record that a user has successfully authenticated at least once,
    /// enabling fail-open during future backend outages.
    pub fn mark_user_known(&self, username: &str) {
        let known_key = format!("known:{}", username);
        self.cache.insert(known_key, true);
    }
}

fn hash_password_fast(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    const_hex::encode(h.finalize())
}
