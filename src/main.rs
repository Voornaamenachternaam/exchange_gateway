// src/main.rs

use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod autodiscover;
mod caldav;
mod calendar;
mod config;
mod eas;
mod ews;
mod ews_folders;
mod ews_update;
mod models;
mod storage;
mod sync;
mod timezone;
mod wbxml;

use crate::config::Config;
use crate::models::AppState;
use crate::storage::Storage;

/// Maximum permitted request body size (4 MiB).
/// All legitimate Outlook EWS/EAS XML payloads are well within this limit.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Middleware: reject bodies whose Content-Length exceeds MAX_BODY_BYTES.
use axum::{
    Router,
    extract::Request,
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tracing_subscriber::EnvFilter;

mod autodiscover;
mod caldav;
mod calendar;
mod config;
mod eas;
mod ews;
mod ews_folders;
mod ews_update;
mod models;
mod storage;
mod sync;
mod timezone;
mod wbxml;

use crate::config::Config;
use crate::models::AppState;
use crate::storage::Storage;

/// Maximum permitted request body size (4 MiB).
/// All legitimate Outlook EWS/EAS XML payloads are well within this limit.
/// Enforced via `RequestBodyLimitLayer`, which caps both fixed-length and chunked bodies.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Autodiscover route handlers (thin wrappers over autodiscover.rs functions)
// ---------------------------------------------------------------------------

async fn autodiscover_xml(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    body: String,
) -> Response {
    let host = &state.cfg.gateway_host;
    let email = autodiscover::extract_email_from_body_xml(&body).unwrap_or_default();
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_xml(host, &body, &email);
    build_response(status, &hdrs, body_out)
}

async fn autodiscover_soap(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    body: String,
) -> Response {
    let host = &state.cfg.gateway_host;
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_soap(host, &body);
    build_response(status, &hdrs, body_out)
}

async fn autodiscover_json(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    axum::extract::Query(params): axum::extract::Query<autodiscover::AutodiscoverJsonParams>,
) -> Response {
    let host = &state.cfg.gateway_host;
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_json(
        host,
        params.protocol.as_deref(),
        params.email.as_deref(),
    );
    build_response(status, &hdrs, body_out)
}

fn build_response(
    status: StatusCode,
    hdrs: &[(&'static str, &'static str)],
    body: String,
) -> Response {
    let mut resp = (status, body).into_response();
    for (k, v) in hdrs {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(k.as_bytes()),
            header::HeaderValue::from_str(v),
        ) {
            resp.headers_mut().insert(name, value);
        }
    }
    resp
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = match Config::load("/etc/exchange-gateway/config.toml") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("CRITICAL: Failed to load config: {}", e);
            return Err(e);
        }
    };

    tracing::info!(
        "Exchange Gateway starting. bind={} gateway_host={}",
        config.bind,
        config.gateway_host
    );

    let storage = Arc::new(Storage::new(&config.worker_url, &config.worker_secret)?);
    let app_state = Arc::new(AppState {
        cfg: config.clone(),
        storage: storage.clone(),
    });

    let app = Router::new()
        .route("/health", get(|| async { StatusCode::OK }))

    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}
