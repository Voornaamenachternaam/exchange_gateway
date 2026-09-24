// src/rate_limit.rs
//! Per-principal request rate limiting using governor.
//!
//! Rate limiting is keyed by the authenticated account (the Basic-auth
//! username), falling back to the client IP reported by the edge
//! (`CF-Connecting-IP`, then the first `X-Forwarded-For` entry) and finally
//! to a global bucket for unidentified traffic. A single global bucket can
//! not be used: all traffic arrives from Cloudflare edge / Microsoft
//! datacenter IPs, so a global limiter would make the target clients
//! (New Outlook for Windows, Outlook Android) throttle each other's
//! Ping/Sync streams into random 429s and self-DoS.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use governor::clock::{Clock, DefaultClock};
use std::convert::Infallible;
use std::sync::Arc;
use tracing::warn;

use crate::models::AppState;

/// Bucket for requests carrying no authenticatable or IP-based identity.
const GLOBAL_KEY: &str = "__global__";

/// Derive the rate-limit key for a request: authenticated username, else
/// client IP from the edge headers, else a shared global bucket.
fn rate_limit_key(headers: &header::HeaderMap) -> String {
    if let Some(user) = basic_auth_username(headers) {
        return format!("user:{user}");
    }
    if let Some(ip) = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get(header::FORWARDED.as_str())
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').find_map(|entry| {
                    entry
                        .split(',')
                        .next()
                        .and_then(|pair| pair.trim().strip_prefix("for="))
                }))
                .map(|s| s.trim_matches('"').trim())
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(',').next())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
    {
        return format!("ip:{ip}");
    }
    GLOBAL_KEY.to_string()
}

/// Extract the username portion of an HTTP Basic `Authorization` header.
fn basic_auth_username(headers: &header::HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = value
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("basic "))
        .and_then(|_| value.get(6..))?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .ok()?;
    let credential = String::from_utf8(decoded).ok()?;
    let (user, _) = credential.split_once(':').unwrap_or((&credential, ""));
    (!user.is_empty()).then(|| user.to_string())
}

/// Middleware that applies per-principal rate limiting to all requests.
pub async fn check_rate_limit(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Infallible> {
    // Skip rate limiting for health and metrics endpoints
    let path = req.uri().path().to_string();
    if path == "/health" || path == "/metrics" {
        return Ok(next.run(req).await);
    }

    // If rate limiting is disabled or no limiter configured, pass through immediately.
    if let Some(limiter) = &state.rate_limiter {
        let key = rate_limit_key(req.headers());
        match limiter.check_key(&key) {
            Ok(()) => {
                let response = next.run(req).await;
                Ok(response)
            }
            Err(not_until) => {
                let wait_ms = not_until
                    .wait_time_from(DefaultClock::default().now())
                    .as_millis();
                warn!(
                    target: "rate_limit",
                    key = %key,
                    wait_ms = wait_ms,
                    "Rate limit exceeded"
                );
                state
                    .metrics
                    .http
                    .request_rejections
                    .with_label_values(&["rate_limit", &key])
                    .inc();
                // Retry-After in seconds (minimum 1)
                let retry_after = wait_ms.div_ceil(1000).max(1);
                let response = (
                    StatusCode::TOO_MANY_REQUESTS,
                    [(header::RETRY_AFTER, retry_after.to_string())],
                    format!(
                        "Rate limit exceeded. Please retry after {} seconds",
                        retry_after
                    ),
                )
                    .into_response();
                Ok(response)
            }
        }
    } else {
        Ok(next.run(req).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    #[test]
    fn key_prefers_basic_auth_username() {
        let mut headers = HeaderMap::new();
        // "alice:secret" base64
        headers.insert(
            header::AUTHORIZATION,
            "Basic YWxpY2U6c2VjcmV0".parse().unwrap(),
        );
        assert_eq!(rate_limit_key(&headers), "user:alice");
    }

    #[test]
    fn key_falls_back_to_cf_connecting_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        assert_eq!(rate_limit_key(&headers), "ip:203.0.113.7");
    }

    #[test]
    fn key_falls_back_to_first_x_forwarded_for() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.3, 10.0.0.1".parse().unwrap(),
        );
        assert_eq!(rate_limit_key(&headers), "ip:198.51.100.3");
    }

    #[test]
    fn key_is_global_without_identity() {
        let headers = HeaderMap::new();
        assert_eq!(rate_limit_key(&headers), GLOBAL_KEY);
    }

    #[test]
    fn malformed_basic_is_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "Basic !!!notb64".parse().unwrap());
        assert_eq!(rate_limit_key(&headers), GLOBAL_KEY);
    }
}
