// src/main.rs
mod active_sync;
mod config;
mod db;
mod ews;
mod jmap_client;
mod utils;
mod wbxml;

use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::{get, post},
    Router,
};
use config::AppConfig;
use std::net::SocketAddr;
use std::sync::Arc;
use tower_http::trace::TraceLayer;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,exchange_gateway=debug".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = match AppConfig::from_env() {
        Ok(c) => Arc::new(c),
        Err(e) => {
            tracing::error!("Configuration Error: {}", e);
            std::process::exit(1);
        }
    };

    let app = Router::new()
        .route("/Microsoft-Server-ActiveSync", post(handle_active_sync))
        .route("/Microsoft-Server-ActiveSync", get(handle_options))
        .route("/EWS/Exchange.asmx", post(handle_ews))
        .fallback(handle_fallback)
        .layer(TraceLayer::new_for_http())
        .with_state(config);

    let addr = SocketAddr::from(([0, 0, 0, 0], 8134));
    tracing::info!("Exchange Gateway v1.0.2.1 listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_fallback(uri: Uri) -> impl IntoResponse {
    tracing::warn!("Unhandled request: {}", uri);
    (StatusCode::NOT_FOUND, "Not Found")
}

async fn handle_options() -> impl IntoResponse {
    let mut headers = HeaderMap::new();
    headers.insert("MS-Server-ActiveSync", "15.0".parse().unwrap());
    headers.insert(header::ALLOW, "OPTIONS,POST".parse().unwrap());
    (StatusCode::OK, headers, "")
}

async fn handle_active_sync(
    State(config): State<Arc<AppConfig>>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if !auth_header.starts_with("Basic ") {
        return (StatusCode::UNAUTHORIZED, HeaderMap::new(), "Unauthorized".into());
    }

    let xml_request = match wbxml::decode(&body) {
        Ok(xml) => xml,
        Err(e) => {
            tracing::error!("WBXML decode error: {:?}", e);
            return (StatusCode::BAD_REQUEST, HeaderMap::new(), "Bad WBXML".into());
        }
    };

    let response_xml = active_sync::process_request(&config, &xml_request, &headers).await;

    match wbxml::encode(&response_xml) {
        Ok(wbxml_data) => {
            let mut headers = HeaderMap::new();
            headers.insert(header::CONTENT_TYPE, "application/vnd.ms-sync.wbxml".parse().unwrap());
            headers.insert("MS-Server-ActiveSync", "15.0".parse().unwrap());
            (StatusCode::OK, headers, wbxml_data)
        }
        Err(e) => {
            tracing::error!("WBXML encode error: {:?}", e);
            (StatusCode::INTERNAL_SERVER_ERROR, HeaderMap::new(), "Encoding Error".into())
        }
    }
}

async fn handle_ews(
    State(config): State<Arc<AppConfig>>,
    headers: HeaderMap,
    body: bytes::Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if !auth_header.starts_with("Basic ") {
        return (StatusCode::UNAUTHORIZED, HeaderMap::new(), "Unauthorized".into());
    }

    let xml_request = match std::str::from_utf8(&body) {
        Ok(s) => s.to_string(),
        Err(_) => return (StatusCode::BAD_REQUEST, HeaderMap::new(), "Invalid UTF-8".into()),
    };

    let response_xml = ews::process_request(&config, &xml_request, &headers).await;
    (StatusCode::OK, HeaderMap::new(), response_xml.into_bytes())
}
