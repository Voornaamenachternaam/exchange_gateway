// src/main.rs
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use tokio::net::TcpListener;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer, limit::RequestBodyLimitLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer, set_header::SetResponseHeaderLayer,
    timeout::RequestBodyTimeoutLayer, trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

use exchange_gateway::{
    autodiscover, config::Config, eas, ews, models::AppState, storage::Storage,
};

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
            eprintln!(
                "CRITICAL: Failed to load config from {}: {}",
                config_path, e
            );
            return Err(e);
        }
    };

    tracing::info!(
        "Exchange Gateway starting. bind={} gateway_host={}",
        config.bind,
        config.gateway_host,
    );

    let storage =
        Arc::new(Storage::new(&format!("sqlite://{}?mode=rwc", config.database_path)).await?);
    storage.init_schema().await?;

    let app_state = Arc::new(AppState::new(config.clone(), storage));

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/EWS/Exchange.asmx", post(ews::handle))
        .route("/EWS/{*path}", post(ews::handle))
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
                ]))
                .layer(TraceLayer::new_for_http())
                .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(
                    REQUEST_TIMEOUT_SECS,
                )))
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
                .layer(CompressionLayer::new())
                .layer(SetResponseHeaderLayer::overriding(
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                ))
                .layer(SetResponseHeaderLayer::overriding(
                    header::HeaderName::from_static("x-frame-options"),
                    HeaderValue::from_static("DENY"),
                ))
                .layer(SetResponseHeaderLayer::overriding(
                    header::REFERRER_POLICY,
                    HeaderValue::from_static("strict-origin-when-cross-origin"),
                ))
                .layer(SetResponseHeaderLayer::overriding(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
                ))
                .layer(SetResponseHeaderLayer::overriding(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("private, no-store, no-cache, max-age=0"),
                ))
                .layer(SetResponseHeaderLayer::overriding(
                    header::HeaderName::from_static("strict-transport-security"),
                    HeaderValue::from_static("max-age=63072000; includeSubDomains"),
                )),
        )
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;

    serve_plain(addr, app).await?;

    tracing::info!("Server shutdown complete");
    Ok(())
}

async fn serve_plain(addr: SocketAddr, app: Router) -> anyhow::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Listening on {} (HTTP)", addr);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        match signal::ctrl_c().await {
            Ok(()) => {}
            Err(err) => {
                tracing::error!("Failed to listen for Ctrl+C: {err}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!("Failed to install signal handler: {e}");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = async { std::future::pending::<()>() };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("Shutdown signal received");
}

async fn health_check(State(state): State<Arc<AppState>>) -> Response {
    // First check database connectivity
    if let Err(e) = state.storage.get_latest_change_seq().await {
        tracing::warn!("Health check: Database connectivity failed: {}", e);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Database unavailable: {}", e),
        )
            .into_response();
    }

    // Optionally check CalDAV if configured (lightweight PROPFIND)
    if !state.cfg.caldav_base.is_empty() {
        match verify_caldav_health(&state).await {
            Ok(_) => (StatusCode::OK, "OK").into_response(),
            Err(e) => {
                tracing::warn!("Health check: CalDAV connectivity failed: {}", e);
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("CalDAV backend unavailable: {}", e),
                )
                    .into_response()
            }
        }
    } else {
        (StatusCode::OK, "OK").into_response()
    }
}

async fn verify_caldav_health(state: &Arc<AppState>) -> Result<()> {
    use exchange_gateway::caldav::CaldavClient;
    // Use a test username that likely doesn't exist - we expect 401 or 404, not connection failure
    let test_user = "health-check";
    let caldav = CaldavClient::new(&state.cfg)?;
    let home_url = format!(
        "{}/cal/{}/",
        state.cfg.caldav_base.trim_end_matches('/'),
        test_user
    );
    let propfind_body = r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop><D:resourcetype/></D:prop>
</D:propfind>"#;

    // Use HEAD or a very light request; we only care that server responds
    let resp = caldav
        .client()
        .request(reqwest::Method::from_bytes(b"PROPFIND")?, &home_url)
        .header("Depth", "0")
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(propfind_body)
        .send()
        .await?;

    // Accept any 2xx or 401/403/404 as "server is reachable"
    // We don't want health check to fail due to auth, just connectivity
    let status = resp.status();
    if status.is_success()
        || status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::NOT_FOUND
    {
        Ok(())
    } else {
        Err(anyhow::anyhow!("Unexpected status: {}", status))
    }
}
