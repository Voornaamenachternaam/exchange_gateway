// src/main.rs
mod active_sync;
mod config;
mod db;
mod ews;
mod jmap_client;
mod utils;
mod wbxml;

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use tower_http::trace::TraceLayer;
use tracing::info;

use crate::config::AppConfig;

#[tokio::main]
async fn main() {
    let env_filter = match tracing_subscriber::EnvFilter::try_from_default_env() {
        Ok(filter) => filter,
        Err(_) => "info,exchange_gateway=debug"
            .parse()
            .expect("valid default filter"),
    };

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    let config = Arc::new(match AppConfig::from_env() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Configuration Error: {}", e);
            std::process::exit(1);
        }
    });
    info!("Exchange Gateway v{} started", env!("CARGO_PKG_VERSION"));

    let app = Router::new()
        .route(
            "/Microsoft-Server-ActiveSync",
            post(handle_active_sync).options(handle_activesync_options),
        )
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
    State(config): State<Arc<AppConfig>>,
    Query(query): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if !auth_header.to_ascii_lowercase().starts_with("basic ") {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="exchange_gateway""#,
            )],
            "Unauthorized".to_string(),
        )
            .into_response();
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok());

    let is_explicit_wbxml = content_type
        .map(|ct| ct.to_ascii_lowercase().contains("wbxml"))
        .unwrap_or(false);

    let is_explicit_xml = content_type
        .map(|ct| {
            let lower = ct.to_ascii_lowercase();
            lower.contains("xml") && !lower.contains("wbxml")
        })
        .unwrap_or(false);

    let (xml_body, is_wbxml) = if is_explicit_xml {
        // Explicitly marked as XML — parse as UTF-8 text
        match std::str::from_utf8(&body) {
            Ok(s) => (s.to_string(), false),
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "Invalid UTF-8".to_string()).into_response();
            }
        }
    } else if is_explicit_wbxml {
        // Explicit WBXML content-type — decode must succeed or return 400
        if body.is_empty() {
            (String::new(), true)
        } else {
            match wbxml::decode(&body) {
                Ok(xml) => (xml, true),
                Err(e) => {
                    tracing::error!("WBXML decode error: {:?}", e);
                    return (
                        StatusCode::BAD_REQUEST,
                        "Unable to decode request body".to_string(),
                    )
                        .into_response();
                }
            }
        }
    } else if body.is_empty() {
        // Allow empty bodies through — some ActiveSync commands legitimately send no body.
        (String::new(), true)
    } else {
        // No Content-Type: sniff by first meaningful byte — WBXML never starts with '<'
        let trimmed = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&body);
        let first_meaningful = trimmed.iter().find(|b| !b.is_ascii_whitespace());
        if first_meaningful == Some(&b'<') {
            match std::str::from_utf8(trimmed) {
                Ok(s) => (s.to_string(), false),
                Err(_) => {
                    return (StatusCode::BAD_REQUEST, "Invalid UTF-8".to_string()).into_response();
                }
            }
        } else {
            match wbxml::decode(trimmed) {
                Ok(xml) => (xml, true),
                Err(e) => {
                    tracing::error!("WBXML decode error: {:?}", e);
                    return (
                        StatusCode::BAD_REQUEST,
                        "Unable to decode request body".to_string(),
                    )
                        .into_response();
                }
            }
        }
    };

    let query_cmd = query.get("Cmd").cloned().unwrap_or_default();
    let response_xml = active_sync::process_request(&config, &xml_body, &headers, &query_cmd).await;

    if is_wbxml {
        match wbxml::encode(&response_xml) {
            Ok(wbxml_data) => (
                StatusCode::OK,
                [
                    ("content-type", "application/vnd.ms-sync.wbxml"),
                    ("MS-Server-ActiveSync", "15.0"),
                ],
                wbxml_data,
            )
                .into_response(),
            Err(e) => {
                tracing::error!("WBXML Encode Error: {}", e);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [
                        ("content-type", "text/plain; charset=utf-8"),
                        ("MS-Server-ActiveSync", "15.0"),
                    ],
                    "WBXML Encode Error".to_string(),
                )
                    .into_response()
            }
        }
    } else {
        (
            StatusCode::OK,
            [
                ("content-type", "application/xml; charset=utf-8"),
                ("MS-Server-ActiveSync", "15.0"),
            ],
            response_xml,
        )
            .into_response()
    }
}

            (header::ALLOW, "POST, OPTIONS"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "POST, OPTIONS"),
            (
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Authorization, Content-Type, X-MS-DeviceId",
            ),
            (header::ACCESS_CONTROL_MAX_AGE, "86400"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        "",
    )
}

async fn handle_ews(
    State(config): State<Arc<AppConfig>>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    if !auth_header.to_ascii_lowercase().starts_with("basic ") {
        return (
            StatusCode::UNAUTHORIZED,
            [(
                header::WWW_AUTHENTICATE,
                r#"Basic realm="exchange_gateway""#,
            )],
            "Unauthorized".to_string(),
        )
            .into_response();
    }

    let xml_body = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                [(header::CONTENT_TYPE, "text/plain")],
                "Invalid UTF-8".to_string(),
            )
                .into_response();
        }
    };

    let response_xml = ews::process_request(&config, xml_body, &headers).await;
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/xml; charset=utf-8")],
        response_xml,
    )
        .into_response()
}
