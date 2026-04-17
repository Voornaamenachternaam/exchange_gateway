// src/main.rs
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use opentelemetry::{KeyValue, global, trace::TracerProvider};
use opentelemetry_otlp::WithExportConfig;
use tokio::net::TcpListener;
use tower::ServiceBuilder;
use tower_http::{
    compression::CompressionLayer, limit::RequestBodyLimitLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer, set_header::SetResponseHeaderLayer,
    timeout::RequestBodyTimeoutLayer, trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

// Use modules from the library crate instead of re-declaring them
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
    // Initialize OpenTelemetry if OTEL_EXPORTER_OTLP_ENDPOINT is set
    // This also initializes the tracing subscriber with the OpenTelemetry layer
    let _otel_guard = init_telemetry()?;

    // If OpenTelemetry was not initialized, set up basic tracing
    if _otel_guard.is_none() {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_default_env())
            .init();
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
                // Security: Redact sensitive headers from logs
                .layer(SetSensitiveRequestHeadersLayer::new([
                    header::AUTHORIZATION,
                    header::HeaderName::from_static("x-gateway-secret"),
                ]))
                // Observability (applied first to capture all requests/responses)
                .layer(TraceLayer::new_for_http())
                // Security: Request timeout
                .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(
                    REQUEST_TIMEOUT_SECS,
                )))
                // Security: Request body size limit
                .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
                // Performance: Response compression
                .layer(CompressionLayer::new())
                // Security headers (applied last so they cover all responses,
                // including error responses from timeout/limit layers)
                // Note: X-XSS-Protection is intentionally omitted as it's deprecated
                // in modern browsers and CSP provides adequate protection
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
                )),
        )
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Listening on {}", addr);
    axum::serve(listener, app).await?;
    Ok(())
}

/// Initialize OpenTelemetry tracing with OTLP exporter.
/// Returns a guard that should be kept alive for the duration of the program.
fn init_telemetry() -> anyhow::Result<Option<opentelemetry_sdk::trace::SdkTracerProvider>> {
    // Initialize basic tracing first
    // Initialize basic tracing without OpenTelemetry

    // Only initialize OpenTelemetry if endpoint is configured
    let endpoint = match std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        Ok(e) => e,
        Err(_) => {
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::from_default_env())
                .init();
            tracing::info!("OpenTelemetry not configured (OTEL_EXPORTER_OTLP_ENDPOINT not set)");
            return Ok(None);
        }
    };

    let service_name =
        std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "exchange-gateway".to_string());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(format!("{}/v1/traces", endpoint))
        .build()?;

    let tracer_provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name(service_name.clone())
                .with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION")))
                .build(),
        )
        .with_batch_exporter(exporter)
        .build();

    let _tracer = tracer_provider.tracer("exchange-gateway");

    // Set global tracer provider
    global::set_tracer_provider(tracer_provider.clone());

    // Initialize with fmt layer only (OpenTelemetry layer has compatibility issues)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    tracing::info!("OpenTelemetry tracing initialized (endpoint: {})", endpoint);

    Ok(Some(tracer_provider))
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
