mod active_sync;
mod config;
mod db;
mod ews;
mod jmap_client;
mod utils;
mod wbxml;

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::AppConfig;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = AppConfig::from_env().expect("Failed to load configuration");
    info!("Exchange Gateway v{} started", env!("CARGO_PKG_VERSION"));

    let app = Router::new()
        .route("/Microsoft-Server-ActiveSync", post(handle_active_sync))
        .route("/EWS/Exchange.asmx", post(handle_ews))
        .route("/health", get(|| async { "OK" }))
        .layer(TraceLayer::new_for_http())
        .with_state(config.clone());

    let addr = "0.0.0.0:8134";
    info!("Listening on {}", addr);

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handle_active_sync(
    State(config): State<AppConfig>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("application/xml");

    let (xml_body, is_wbxml) = if content_type.contains("wbxml") {
        match wbxml::decode(&body) {
            Ok(xml) => (xml, true),
            Err(e) => {
                tracing::error!("WBXML Decode Error: {}", e);
                return (StatusCode::BAD_REQUEST, "WBXML Decode Error".to_string()).into_response();
            }
        }
    } else {
        match std::str::from_utf8(&body) {
            Ok(s) => (s.to_string(), false),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "Invalid UTF-8".to_string()).into_response();
            }
        }
    };

    let response_xml = active_sync::process_request(&config, &xml_body, &headers).await;

    if is_wbxml {
        match wbxml::encode(&response_xml) {
            Ok(wbxml_data) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/vnd.ms-sync.wbxml")],
                wbxml_data,
            )
                .into_response(),
            Err(e) => {
                tracing::error!("WBXML Encode Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "WBXML Encode Error".to_string(),
                )
                    .into_response()
            }
        }
    } else {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
            response_xml,
        )
            .into_response()
    }
}

async fn handle_ews(
    State(config): State<AppConfig>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let xml_body = match std::str::from_utf8(&body) {
        Ok(s) => s,
        // Fix: Match the tuple structure of the success branch (Status, Header, Body)
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain")],
                "Invalid UTF-8".to_string(),
            );
        }
    };

    let response_xml = ews::process_request(&config, xml_body, &headers).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        response_xml,
    )
}
