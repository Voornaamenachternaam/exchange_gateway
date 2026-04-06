// src/main.rs
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Router,
};
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{
    cors::{Any, CorsLayer},
    sensitive_headers::SetSensitiveRequestHeadersLayer,
    trace::TraceLayer,
    request_id::MakeRequestUuid,
    timeout::RequestBodyTimeoutLayer,
    limit::RequestBodyLimitLayer,
    compression::CompressionLayer,
};
use tracing_subscriber::EnvFilter;

mod autodiscover;
mod caldav;
mod calendar;
mod config;
mod eas;
mod error;
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

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 60;

async fn autodiscover_xml(State(state): State<Arc<AppState>>, body: String) -> Response {
    let host = &state.cfg.gateway_host;
    let email = autodiscover::extract_email_from_body_xml(&body).unwrap_or_default();
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_xml(host, &body, &email);
    build_response(status, &hdrs, body_out)
}

async fn autodiscover_soap(State(state): State<Arc<AppState>>, body: String) -> Response {
    let host = &state.cfg.gateway_host;
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_soap(host, &body);
    build_response(status, &hdrs, body_out)
}

async fn autodiscover_json(
    State(state): State<Arc<AppState>>,
    Query(params): Query<autodiscover::AutodiscoverJsonParams>,
) -> Response {
    let host = &state.cfg.gateway_host;
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_json(
        host,
        params.protocol.as_deref(),
        params.email.as_deref(),
    );
    build_response(status, &hdrs, body_out)
}

fn build_response(status: StatusCode, hdrs: &[(&'static str, &'static str)], body: String) -> Response {
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config_path = std::env::var("GATEWAY_CONFIG")
        .unwrap_or_else(|_| "/etc/exchange-gateway/config.toml".to_string());

    let config = match Config::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("CRITICAL: Failed to load config from {}: {}", config_path, e);
            return Err(e);
        }
    };

    tracing::info!(
        "Exchange Gateway starting. bind={} gateway_host={}",
        config.bind,
        config.gateway_host
    );

    let storage = Arc::new(Storage::new(&config.worker_url, config.worker_secret())?);

    let app_state = Arc::new(AppState {
        cfg: config.clone(),
        storage,
    });

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/EWS/Exchange.asmx", post(ews::handle))
        .route("/EWS/*path", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .route("/autodiscover/autodiscover.xml", post(autodiscover_xml))
        .route("/Autodiscover/Autodiscover.xml", post(autodiscover_xml))
        .route("/autodiscover/autodiscover.svc", post(autodiscover_soap))
        .route("/Autodiscover/Autodiscover.svc", post(autodiscover_soap))
        .route("/autodiscover/autodiscover.json", get(autodiscover_json))
        .route("/Autodiscover/autodiscover.json", get(autodiscover_json))
        .layer(
            ServiceBuilder::new()
                .layer(SetSensitiveRequestHeadersLayer::new([
                    header::AUTHORIZATION,
                    header::HeaderName::from_static("x-gateway-secret"),
                ]))
                .layer(TraceLayer::new_for_http())
                .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(REQUEST_TIMEOUT_SECS)))
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
                .layer(CompressionLayer::new())
        )
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

async fn health_check(State(state): State<Arc<AppState>>) -> Response {
    let worker_ok = match state.storage.get_latest_change_seq().await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("Health check: Worker connectivity failed: {}", e);
            false
        }
    };
    if worker_ok {
        (StatusCode::OK, "OK").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "Worker unavailable").into_response()
    }
}
