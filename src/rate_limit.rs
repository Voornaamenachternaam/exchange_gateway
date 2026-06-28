// src/rate_limit.rs
//! Request rate limiting using governor.
//! Protects the gateway from floods by limiting requests per second globally.

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

/// Middleware that applies global rate limiting to all requests.
pub async fn check_rate_limit(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Infallible> {
    // Skip rate limiting for health and metrics endpoints
    let path = req.uri().path();
    if path == "/health" || path == "/metrics" {
        return Ok(next.run(req).await);
    }

    // If rate limiting is disabled or no limiter configured, pass through immediately.
    if let Some(limiter) = &state.rate_limiter {
        match limiter.check() {
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
                    wait_ms = wait_ms,
                    "Rate limit exceeded"
                );
                state
                    .metrics
                    .http
                    .request_rejections
                    .with_label_values(&["rate_limit", "global"])
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
