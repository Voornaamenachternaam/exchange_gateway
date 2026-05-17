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
use exchange_gateway::{
    autodiscover, config::Config, eas, ews, logging, models::AppState, storage::Storage,
};
use tokio::net::TcpListener;
use tokio::signal;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer, limit::RequestBodyLimitLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer, set_header::SetResponseHeaderLayer,
    timeout::RequestBodyTimeoutLayer, trace::TraceLayer,
};
use tracing::{debug, info, warn};

/// Redact an email address for logging.
/// Shows username and masked domain to preserve some context while protecting PII.
/// Examples: "user@example.com" -> "user@***", "user@sub.example.co.uk" -> "user@***"
fn redact_email(email: &str) -> String {
    if email.is_empty() {
        return String::new();
    }
    // Split on '@' to separate username from domain
    let at_pos = email.find('@');
    match at_pos {
        Some(pos) => {
            let username = &email[..pos];
            // Show username (first part) but mask the domain entirely
            // This preserves some debugging context (which user) without exposing domain
            format!("{}@***", username)
        }
        None => {
            // No '@' means it's not a valid email; show masked prefix (maybe it's a username)
            // Use char iteration to safely handle UTF-8, avoiding panic on multi-byte chars
            let first_char = email.chars().next().unwrap_or('?');
            if email.len() >= 2 {
                format!("{}***", first_char)
            } else {
                "***".to_string()
            }
        }
    }
}

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 60;

async fn autodiscover_xml(State(state): State<Arc<AppState>>, body: String) -> Response {
    let start = std::time::Instant::now();
    let host = &state.cfg.gateway_host;
    let email = autodiscover::extract_email_from_body_xml(&body).unwrap_or_default();

    debug!(
        target: "http",
        method = "POST",
        path = "/autodiscover/autodiscover.xml",
        body_len = body.len(),
        email = %redact_email(&email),
        "Autodiscover XML request received"
    );

    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_xml(host, &body, &email);

    let elapsed_ms = start.elapsed().as_millis();
    let success = status.is_success();

    if success {
        info!(
            target: "http",
            method = "POST",
            path = "/autodiscover/autodiscover.xml",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            response_len = body_out.len(),
            email = %redact_email(&email),
            "Autodiscover XML completed"
        );
    } else {
        warn!(
            target: "http",
            method = "POST",
            path = "/autodiscover/autodiscover.xml",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            email = %redact_email(&email),
            "Autodiscover XML failed"
        );
    }

    build_response(status, &hdrs, body_out)
}

async fn autodiscover_soap(State(state): State<Arc<AppState>>, body: String) -> Response {
    let start = std::time::Instant::now();
    let host = &state.cfg.gateway_host;

    debug!(
        target: "http",
        method = "POST",
        path = "/autodiscover/autodiscover.svc",
        body_len = body.len(),
        "Autodiscover SOAP request received"
    );

    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_soap(host, &body);

    let elapsed_ms = start.elapsed().as_millis();
    let success = status.is_success();

    if success {
        info!(
            target: "http",
            method = "POST",
            path = "/autodiscover/autodiscover.svc",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            response_len = body_out.len(),
            "Autodiscover SOAP completed"
        );
    } else {
        warn!(
            target: "http",
            method = "POST",
            path = "/autodiscover/autodiscover.svc",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            "Autodiscover SOAP failed"
        );
    }

    build_response(status, &hdrs, body_out)
}

async fn autodiscover_json(
    State(state): State<Arc<AppState>>,
    Query(params): Query<autodiscover::AutodiscoverJsonParams>,
) -> Response {
    let start = std::time::Instant::now();
    let host = &state.cfg.gateway_host;

    debug!(
        target: "http",
        method = "GET",
        path = "/autodiscover/autodiscover.json",
        protocol = ?params.protocol,
        email = ?params.email.as_deref().map(redact_email),
        "Autodiscover JSON request received"
    );

    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_json(
        host,
        params.protocol.as_deref(),
        params.email.as_deref(),
    );

    let elapsed_ms = start.elapsed().as_millis();
    let success = status.is_success();

    if success {
        info!(
            target: "http",
            method = "GET",
            path = "/autodiscover/autodiscover.json",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            response_len = body_out.len(),
            protocol = ?params.protocol,
            "Autodiscover JSON completed"
        );
    } else {
        warn!(
            target: "http",
            method = "GET",
            path = "/autodiscover/autodiscover.json",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            protocol = ?params.protocol,
            "Autodiscover JSON failed"
        );
    }

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
    // Initialize advanced logging system with fallback to basic logging on error
    if let Err(e) = logging::init_logging() {
        eprintln!(
            "Failed to initialize logging: {}, falling back to basic stderr logging",
            e
        );
        // Fall back to simple stderr logging with RUST_LOG level
        let level = std::env::var("GATEWAY_LOG_LEVEL")
            .or_else(|_| std::env::var("RUST_LOG"))
            .unwrap_or_else(|_| "info".to_string());
        let filter = tracing_subscriber::EnvFilter::try_new(&level)
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        tracing_subscriber::fmt().with_env_filter(filter).init();
    }

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
    let start = std::time::Instant::now();

    debug!(
        target: "health",
        caldav_configured = !state.cfg.caldav_base.is_empty(),
        "Health check started"
    );

    // First check database connectivity
    if let Err(e) = state.storage.get_latest_change_seq().await {
        let elapsed_ms = start.elapsed().as_millis();
        warn!(
            target: "health",
            status = "unhealthy",
            check = "database",
            elapsed_ms = elapsed_ms,
            error = %e,
            "Health check failed - database unavailable"
        );
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            format!("Database unavailable: {}", e),
        )
            .into_response();
    }

    // Optionally check CalDAV if configured (lightweight PROPFIND)
    if !state.cfg.caldav_base.is_empty() {
        match verify_caldav_health(&state).await {
            Ok(_) => {
                let elapsed_ms = start.elapsed().as_millis();
                info!(
                    target: "health",
                    status = "healthy",
                    elapsed_ms = elapsed_ms,
                    "Health check passed"
                );
                (StatusCode::OK, "OK").into_response()
            }
            Err(e) => {
                let elapsed_ms = start.elapsed().as_millis();
                warn!(
                    target: "health",
                    status = "unhealthy",
                    check = "caldav",
                    elapsed_ms = elapsed_ms,
                    error = %e,
                    "Health check failed - CalDAV backend unavailable"
                );
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("CalDAV backend unavailable: {}", e),
                )
                    .into_response()
            }
        }
    } else {
        let elapsed_ms = start.elapsed().as_millis();
        info!(
            target: "health",
            status = "healthy",
            caldav_check = "skipped",
            elapsed_ms = elapsed_ms,
            "Health check passed (CalDAV not configured)"
        );
        (StatusCode::OK, "OK").into_response()
    }
}

async fn verify_caldav_health(state: &Arc<AppState>) -> Result<()> {
    use exchange_gateway::caldav::CaldavClient;
    let caldav = CaldavClient::new(&state.cfg)?;
    // Use a lightweight OPTIONS request to the CalDAV base URL with dummy
    // Basic auth. This avoids two Stalwart log-noise problems:
    //   (1) "Missing Authorization header" — caused by unauthenticated requests
    //   (2) "invalid credentials" — caused by hitting user-specific paths like
    //       /dav/cal/{username}/ with non-existent users
    // By sending OPTIONS (not PROPFIND) to the base /dav/ path (not a user
    // path) with dummy auth, Stalwart processes it as an authenticated request
    // even though the credentials are invalid, avoiding the "Missing Authorization
    // header" log entry entirely. The base path also avoids user-lookup noise.
    let base_url = state.cfg.caldav_base.trim_end_matches('/').to_string();

    let resp = caldav
        .client()
        .request(reqwest::Method::OPTIONS, &base_url)
        .basic_auth("gateway-health", Some("ping"))
        .send()
        .await?;

    let status = resp.status();
    // Accept any 2xx or 401/403/404/405 as "server is reachable"
    // 401 = server is up, credentials rejected (expected)
    // 405 = OPTIONS not allowed but server is reachable
    // 403/404 = server is up, path not found/forbidden
    if status.is_success()
        || status == StatusCode::UNAUTHORIZED
        || status == StatusCode::FORBIDDEN
        || status == StatusCode::NOT_FOUND
        || status == StatusCode::METHOD_NOT_ALLOWED
    {
        Ok(())
    } else {
        // A 5xx (or any other unexpected status) means the server is
        // unhealthy. We do NOT fall back to a GET request because:
        // (1) The same server returning 5xx on OPTIONS will almost
        //     certainly return 5xx on GET too, doubling latency for
        //     the same failure outcome.
        // (2) If GET somehow returned 2xx after OPTIONS returned 5xx,
        //     that would mask a genuinely unhealthy CalDAV server.
        // Fail fast with a clear message instead.
        warn!(
            target: "health",
            status = status.as_u16(),
            "CalDAV server returned unexpected status on OPTIONS"
        );
        Err(anyhow::anyhow!(
            "CalDAV server returned unexpected status: {}",
            status
        ))
    }
}
