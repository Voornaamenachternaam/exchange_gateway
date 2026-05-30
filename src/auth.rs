// src/auth.rs
use crate::caldav::CaldavClient;
use crate::config::Config;
use crate::jmap::JmapClient;
use moka::sync::Cache;
use secrecy::SecretString;
use std::time::Duration;
use tracing::{debug, trace, warn};

#[derive(Clone)]
pub struct AuthVerifier {
    cache: Cache<String, bool>,
    caldav_base: String,
    jmap_base: String,
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
            jmap_base: cfg.jmap_base.clone(),
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
            "Cache miss - verifying credentials"
        );

        // Try JMAP first (single HTTP endpoint for both email and calendar),
        // then fall back to CalDAV if JMAP is not configured or fails.
        let valid = if !self.jmap_base.is_empty() {
            match JmapClient::new(&self.jmap_base) {
                Ok(client) => {
                    let secret_password = SecretString::from(password.to_string());
                    match client.verify_credentials(username, &secret_password).await {
                        Ok(()) => {
                            debug!(
                                target: "auth",
                                username = %username,
                                "Authenticated via JMAP"
                            );
                            true
                        }
                        Err(e) => {
                            // JMAP auth failed — could be wrong credentials or server error.
                            // Fall through to CalDAV if available.
                            warn!(
                                target: "auth",
                                username = %username,
                                error = %e,
                                "JMAP auth failed, falling back to CalDAV"
                            );
                            self.verify_caldav(username, password).await
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        target: "auth",
                        username = %username,
                        error = %e,
                        "Failed to create JMAP client, falling back to CalDAV"
                    );
                    self.verify_caldav(username, password).await
                }
            }
        } else {
            self.verify_caldav(username, password).await
        };

        // Compute cache_key_len before consuming cache_key on insert to avoid clone
        let cache_key_len = cache_key.len();
        self.cache.insert(cache_key, valid);
        debug!(
            target: "auth",
            username = %username,
            valid = valid,
            cache_key_len = cache_key_len,
            "Authentication result cached"
        );
        valid
    }

    /// Verify credentials via CalDAV PROPFIND.
    async fn verify_caldav(&self, username: &str, password: &str) -> bool {
        let caldav = match CaldavClient::new_from_base(&self.caldav_base) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    target: "auth",
                    username = %username,
                    error = %e,
                    "Failed to create CalDAV client for auth"
                );
                return false;
            }
        };
        caldav.verify_credentials(username, password).await
    }
}

fn hash_password_fast(password: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    const_hex::encode(h.finalize())
}
