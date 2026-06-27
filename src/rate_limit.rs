// src/rate_limit.rs
//! Request rate limiting using governor.
//! Protects the gateway from floods by limiting requests per second globally.

use std::convert::Infallible;
use std::sync::Arc;
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use governor::{
    clock::{Clock, DefaultClock},
};
use tracing::warn;

use crate::models::AppState;

/// Middleware that applies global rate limiting to all requests.
pub async fn check_rate_limit(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, Infallible> {
    match state.rate_limiter.check() {
        Ok(()) => {
            let response = next.run(req).await;
            Ok(response)
        }
        Err(not_until) => {
            let wait_ms = not_until.wait_time_from(DefaultClock::default().now()).as_millis();
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
            let response = (
                StatusCode::TOO_MANY_REQUESTS,
                format!("Rate limit exceeded. Please retry after {} ms", wait_ms),
            )
                .into_response();
            Ok(response)
        }
    }
}

