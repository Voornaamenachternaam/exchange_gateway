// src/rate_limit.rs
//! Per-client rate limiting using governor.
//!
//! Buckets are keyed by the client IP as reported by the edge
//! (`CF-Connecting-IP`, then the RFC 7239 `Forwarded` header, then the first
//! `X-Forwarded-For` entry), with a shared global bucket as the final
//! fallback. A single global bucket can not be used: all traffic arrives
//! from Cloudflare edge / Microsoft datacenter IPs, so a global limiter
//! would make the target clients (New Outlook for Windows, Outlook Android)
//! throttle each other's Ping/Sync streams into random 429s and self-DoS.
//!
//! The bucket key is deliberately *not* the Basic-auth username:
//! `check_rate_limit` runs as outer middleware, before any handler has
//! verified the credentials, so bucketing on the raw `Authorization` header
//! would let an unauthenticated caller claim another user's bucket
//! (starving that user) or rotate through unlimited synthetic `user:`
//! buckets. The edge-provided client IP cannot be forged this way — the
//! reference deployment fronts the gateway exclusively through the
//! cloudflared tunnel, and Cloudflare overwrites `CF-Connecting-IP` for all
//! proxied traffic. Direct exposure without Cloudflare is unsupported (per
//! `CLOUDFLARED_SETUP.md`); on such a path the oldest-client-first
//! `X-Forwarded-For` chain is client-controlled, which attackers can already
//! rotate trivially, so keying on it is no worse than any other pre-auth
//! signal.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::clock::{Clock, DefaultClock};
use std::convert::Infallible;
use std::sync::Arc;
use tracing::warn;

use crate::models::AppState;

/// Bucket for requests carrying no usable client-IP identity.
const GLOBAL_KEY: &str = "__global__";

/// Return the rate-limit key and its bounded category label for a request.
///
/// The category (`"ip"` or `"global"`) is safe to use as a Prometheus label
/// value; the key itself is high-cardinality (an IP address) and must only
/// appear in structured logs, never in metric labels.
fn rate_limit_key(headers: &header::HeaderMap) -> (String, &'static str) {
    let ip = headers
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
        });
    match ip {
        Some(ip) => (format!("ip:{ip}"), "ip"),
        None => (GLOBAL_KEY.to_string(), "global"),
    }
}

/// Middleware that applies per-client rate limiting to all requests.
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
        let (key, category) = rate_limit_key(req.headers());
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
                // The label must stay low-cardinality: one series per
                // bounded category, never one per IP (each rejected key
                // would otherwise create a new metric series).
                state
                    .metrics
                    .http
                    .request_rejections
                    .with_label_values(&["rate_limit", category])
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
    fn key_prefers_cf_connecting_ip() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "203.0.113.7".parse().unwrap());
        headers.insert(
            "x-forwarded-for",
            "198.51.100.3, 10.0.0.1".parse().unwrap(),
        );
        assert_eq!(rate_limit_key(&headers), ("ip:203.0.113.7".into(), "ip"));
    }

    #[test]
    fn key_falls_back_to_rfc7239_forwarded() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::FORWARDED,
            "for=\"198.51.100.9\";proto=https".parse().unwrap(),
        );
        assert_eq!(
            rate_limit_key(&headers),
            ("ip:198.51.100.9".into(), "ip")
        );
    }

    #[test]
    fn key_falls_back_to_first_x_forwarded_for() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            "198.51.100.3, 10.0.0.1".parse().unwrap(),
        );
        assert_eq!(rate_limit_key(&headers), ("ip:198.51.100.3".into(), "ip"));
    }

    #[test]
    fn key_is_global_without_identity() {
        let headers = HeaderMap::new();
        assert_eq!(rate_limit_key(&headers), (GLOBAL_KEY.into(), "global"));
    }

    #[test]
    fn authorization_header_does_not_select_bucket() {
        let mut headers = HeaderMap::new();
        // "alice:secret" — must not influence keying before verification.
        headers.insert(
            header::AUTHORIZATION,
            "Basic YWxpY2U6c2VjcmV0".parse().unwrap(),
        );
        assert_eq!(rate_limit_key(&headers), (GLOBAL_KEY.into(), "global"));
    }

    #[test]
    fn empty_ip_values_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("cf-connecting-ip", "".parse().unwrap());
        assert_eq!(rate_limit_key(&headers), (GLOBAL_KEY.into(), "global"));
    }
}
