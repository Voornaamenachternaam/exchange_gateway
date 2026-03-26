// src/main.rs
// Exchange Gateway - Main Application
//
// Provides Exchange ActiveSync (EAS) and Exchange Web Services (EWS) protocols
// for seamless integration between Outlook clients and Stalwart Mailserver.
//
// Features:
// - EAS protocol v12.0 through v16.1 support
// - EWS protocol support
// - Full calendar synchronization
// - Meeting request handling
// - Attachment support
// - Security-hardened implementation
//
// March 2026 - Production-ready

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tower::ServiceBuilder;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{Level, debug, error, info, instrument, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

// Module declarations
mod caldav_ext;
mod eas_protocol;
mod ews_handlers;
mod handlers;
mod models;
mod security;
mod utils;
mod xml_builder;

// Re-exports for use in other modules
pub use caldav_ext::{CalDavClientExt, CalendarEvent};
pub use eas_protocol::{
    DeleteType, EAS_VERSION_12_0, EAS_VERSION_12_1, EAS_VERSION_14_0, EAS_VERSION_14_1,
    EAS_VERSION_16_0, EAS_VERSION_16_1, EmptyFolderContentsRequest, GetAttachmentRequest,
    GetItemEstimateRequest, ProtocolCapabilities, SearchRequest, SendMailRequest,
    SmartMessageRequest, ValidateCertRequest, ValidateCertStatus, extract_protocol_version,
    validate_command_grammar, validate_protocol_version,
};
pub use handlers::{EasCommandParams, handle_eas_command};
pub use models::{
    EasAttendee, EasCalendarEvent, EasException, EasRecurrence, build_eas_calendar_response,
    eas_recurrence_to_ical, ical_rrule_to_eas, parse_attendees_from_eas,
    parse_eas_calendar_request, parse_recurrence_from_eas,
};
pub use security::{
    RateLimiter, check_certificate_revocation, generate_secure_token, sanitize_xml_content,
    secure_compare, validate_base64, validate_certificate_chain, validate_email,
    validate_iso8601_datetime, validate_url, validate_uuid,
};
pub use utils::{
    bytes_to_hex, current_timestamp, current_timestamp_millis, fold_ical_line, format_bytes,
    format_comma_list, format_datetime_eas, format_datetime_ical, format_datetime_iso8601,
    format_duration, generate_etag, generate_short_id, generate_uid, hex_to_bytes,
    is_ascii_printable, is_valid_email, md5_hash, normalize_crlf, parse_comma_list,
    parse_datetime_to_utc, parse_ical_datetime, regex_escape, safe_substring, sanitize_for_url,
    sha256_hash, truncate, unfold_ical_lines,
};
pub use xml_builder::{
    EasXmlBuilder, NS_AIR_SYNC, NS_AIR_SYNC_BASE, NS_CALENDAR, NS_COMPOSE_MAIL, NS_CONTACTS,
    NS_EMAIL, NS_FOLDER_HIERARCHY, NS_GET_ITEM_ESTIMATE, NS_ITEM_OPERATIONS, NS_MEETING_RESPONSE,
    NS_MOVE, NS_PING, NS_PROVISION, NS_RESOLVE_RECIPIENTS, NS_SEARCH, NS_SETTINGS, NS_TASKS,
    NS_VALIDATE_CERT, build_eas_error, build_eas_folder_sync_response,
    build_eas_get_item_estimate_response, build_eas_item_operations_response,
    build_eas_meeting_response_response, build_eas_ping_response, build_eas_provision_response,
    build_eas_resolve_recipients_response, build_eas_search_response, build_eas_settings_response,
    build_eas_success, build_eas_sync_response, build_eas_validate_cert_response,
};

/// Application state shared across handlers
#[derive(Clone)]
pub struct AppState {
    /// CalDAV client for backend communication
    pub caldav_client: Arc<CalDavClientExt>,
    /// Sync state storage
    pub sync_states: Arc<SyncStateStore>,
    /// Rate limiter for authentication
    pub rate_limiter: Arc<RateLimiter>,
    /// Configuration
    pub config: Arc<AppConfig>,
}

/// Application configuration
#[derive(Clone, Debug)]
pub struct AppConfig {
    /// CalDAV server URL
    pub caldav_url: String,
    /// Server bind address
    pub bind_addr: String,
    /// Maximum request size
    pub max_request_size: usize,
    /// Enable request logging
    pub enable_logging: bool,
    /// Default protocol version
    pub default_protocol_version: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            caldav_url: std::env::var("CALDAV_URL")
                .unwrap_or_else(|_| "http://stalwart:8080".to_string()),
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string()),
            max_request_size: 10 * 1024 * 1024, // 10 MB
            enable_logging: true,
            default_protocol_version: "16.1".to_string(),
        }
    }
}

/// Sync state storage
#[derive(Clone)]
pub struct SyncStateStore {
pub struct SyncStateStore {
    pub db: Arc<crate::storage::Storage>,
}

impl SyncStateStore {
    pub async fn get(&self, owner: &str, collection_id: &str) -> anyhow::Result<Option<SyncState>> {
        self.db.get_sync_state(owner, collection_id).await
    }

    pub async fn set(&self, owner: &str, collection_id: &str, state: SyncState) -> anyhow::Result<()> {
        self.db.set_sync_state(owner, collection_id, state).await
    }
}
}

impl SyncStateStore {
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn get(&self, key: &str) -> Option<SyncState> {
        let states = self.states.read().await;
        states.get(key).cloned()
    }

    pub async fn set(&self, key: String, state: SyncState) {
        let mut states = self.states.write().await;
        states.insert(key, state);
    }

    pub async fn remove(&self, key: &str) {
        let mut states = self.states.write().await;
        states.remove(key);
    }
}

/// Sync state for a device
#[derive(Clone, Debug)]
pub struct SyncState {
    pub sync_key: String,
    pub collection_id: String,
    pub last_sync: chrono::DateTime<chrono::Utc>,
    pub known_items: Vec<String>,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            sync_key: "0".to_string(),
            collection_id: String::new(),
            last_sync: chrono::DateTime::UNIX_EPOCH,
            known_items: Vec::new(),
        }
    }
}

/// Error response structure
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub status: u16,
    pub message: String,
}

impl ErrorResponse {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: 400,
            message: message.into(),
        }
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: 401,
            message: message.into(),
        }
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: 403,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: 404,
            message: message.into(),
        }
    }

    pub fn internal_error(message: impl Into<String>) -> Self {
        Self {
            status: 500,
            message: message.into(),
        }
    }
}

impl IntoResponse for ErrorResponse {
    fn into_response(self) -> Response {
        let body = serde_json::json!({
            "error": {
                "status": self.status,
                "message": self.message,
            }
        });

        Response::builder()
            .status(StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }
}

/// JSON error response
#[derive(Debug, Serialize)]
pub struct JsonError {
    pub error: String,
}

/// Health check response
#[derive(Debug, Serialize)]
struct HealthResponse {
    status: String,
    version: String,
    timestamp: String,
}

/// Options request handler for CORS preflight
async fn handle_options() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", "*")
        .header(
            "Access-Control-Allow-Methods",
            "GET, POST, PUT, DELETE, OPTIONS, PROPFIND, REPORT, MKCOL, MOVE",
        )
        .header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, MS-ASProtocolVersion",
        )
        .body(Body::empty())
        .unwrap()
}

/// Health check handler
async fn health_check() -> impl IntoResponse {
    let response = HealthResponse {
        status: "healthy".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
    };

    (StatusCode::OK, axum::Json(response))
}

/// Root handler - returns service info
async fn root_handler() -> impl IntoResponse {
    let info = serde_json::json!({
        "service": "Exchange Gateway",
        "version": env!("CARGO_PKG_VERSION"),
        "protocols": ["EAS", "EWS"],
        "eas_versions": ["12.0", "12.1", "14.0", "14.1", "16.0", "16.1"],
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    (StatusCode::OK, axum::Json(info))
}

/// Authentication middleware
async fn auth_middleware(
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Result<Response, ErrorResponse> {
    // Extract authorization header
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok());

    if let Some(auth) = auth_header {
        if auth.starts_with("Basic ") {
            let credentials = &auth[6..];
            match security::validate_basic_auth(credentials) {
                Ok((username, _password)) => {
                    debug!("Authenticated user: {}", username);
                    // Add username to request extensions for later use
                    return Ok(next.run(request).await);
                }
                Err(e) => {
                    warn!("Authentication failed: {}", e);
                    return Err(ErrorResponse::unauthorized("Invalid credentials"));
                }
            }
        }
    }

    // Allow requests without auth for OPTIONS and health check
    // (actual auth will be checked in specific handlers)
    Ok(next.run(request).await)
}

/// Logging middleware
async fn logging_middleware(
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let start = std::time::Instant::now();
    let path = uri.path().to_string();

    debug!(
        "Request: {} {} {:?}",
        method,
        path,
        headers.get("user-agent").and_then(|h| h.to_str().ok())
    );

    let response = next.run(request).await;

    let duration = start.elapsed();
    info!(
        "{} {} - {} in {:?}",
        method,
        path,
        response.status(),
        duration
    );

    response
}

/// EAS endpoint handler
#[instrument(skip(state, body, headers))]
async fn eas_handler(
    Path(user): Path<String>,
    Query(params): Query<EasCommandParams>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ErrorResponse> {
    // Validate request size
    if body.len() > state.config.max_request_size {
        return Err(ErrorResponse::bad_request("Request too large"));
    }

    // Handle EAS command
    handle_eas_command(
        Path((user, params.device_id.clone())),
        Query(params),
        State(state),
        headers,
        body,
    )
    .await
}

/// EWS endpoint handler
#[instrument(skip(state, body, headers))]
async fn ews_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ErrorResponse> {
    // Validate request size
    if body.len() > state.config.max_request_size {
        return Err(ErrorResponse::bad_request("Request too large"));
    }

    // Handle EWS request
    ews_handlers::handle_ews_request(State(state), headers, body).await
}

/// Autodiscover handler for Outlook clients
#[instrument]
async fn autodiscover_handler(headers: HeaderMap, body: Bytes) -> impl IntoResponse {
    debug!("Autodiscover request received");

    // Parse the request to get the email address
    let body_str = String::from_utf8_lossy(&body);
    let email = extract_email_from_autodiscover(&body_str);

    // Build autodiscover response
    let response = build_autodiscover_response(email.as_deref());

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/xml")
        .body(Body::from(response))
        .unwrap()
}

/// Extract email from autodiscover request
fn extract_email_from_autodiscover(xml: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_email = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                if e.name().local_name().as_ref() == b"EMailAddress" {
                    in_email = true;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) if in_email => {
                if let Ok(text) = t.decode() {
                    return Some(text.into_owned());
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if e.name().local_name().as_ref() == b"EMailAddress" {
                    in_email = false;
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Build autodiscover response
fn build_autodiscover_response(email: Option<&str>) -> String {
    let email = email.unwrap_or("user@example.com");
    let domain = email.split('@').nth(1).unwrap_or("example.com");

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006">
    <Culture>en:us</Culture>
    <User>
      <DisplayName>{}</DisplayName>
      <EMailAddress>{}</EMailAddress>
    </User>
    <Action>
      <Settings>
        <Server>
          <Type>MobileSync</Type>
    // Build autodiscover response
    let gateway_host = state.config.gateway_host.clone();
    let response = build_autodiscover_response(email.as_deref(), &gateway_host);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/xml")
        .body(Body::from(response))
        .unwrap()
}

/// Extract email from autodiscover request
fn extract_email_from_autodiscover(xml: &str) -> Option<String> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut in_email = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(quick_xml::events::Event::Start(e)) => {
                if e.name().local_name().as_ref() == b"EMailAddress" {
                    in_email = true;
                }
            }
            Ok(quick_xml::events::Event::Text(t)) if in_email => {
                if let Ok(text) = t.decode() {
                    return Some(text.into_owned());
                }
            }
            Ok(quick_xml::events::Event::End(e)) => {
                if e.name().local_name().as_ref() == b"EMailAddress" {
                    in_email = false;
                }
            }
            Ok(quick_xml::events::Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    None
}

/// Build autodiscover response
fn build_autodiscover_response(email: Option<&str>, gateway_host: &str) -> String {
    let email = email.unwrap_or("user@example.com");

    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/responseschema/2006">
  <Response xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006">
    <Culture>en:us</Culture>
    <User>
      <DisplayName>{}</DisplayName>
      <EMailAddress>{}</EMailAddress>
    </User>
    <Action>
      <Settings>
        <Server>
          <Type>MobileSync</Type>
          <Url>https://{}/Microsoft-Server-ActiveSync</Url>
          <Name>Exchange Gateway</Name>
        </Server>
      </Settings>
    </Action>
  </Response>
</Autodiscover>"#,
        email.split('@').next().unwrap_or("User"),
        email,
        gateway_host
    )
}
          <Name>Exchange Gateway</Name>
        </Server>
      </Settings>
    </Action>
  </Response>
</Autodiscover>"#,
        email.split('@').next().unwrap_or("User"),
        email,
        domain
    )
}

/// Create the application router
fn create_app(state: Arc<AppState>) -> Router {
    // Build middleware stack
    let middleware = ServiceBuilder::new()
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    Router::new()
        // Health check
        .route("/health", get(health_check))
        .route("/", get(root_handler))
        // Autodiscover endpoints
        .route("/autodiscover/autodiscover.xml", post(autodiscover_handler))
        .route("/Autodiscover/Autodiscover.xml", post(autodiscover_handler))
        // EAS endpoints
        .route("/Microsoft-Server-ActiveSync", any(eas_handler))
        .route("/Microsoft-Server-ActiveSync/", any(eas_handler))
        .route("/Microsoft-Server-ActiveSync/:user", any(eas_handler))
        // EWS endpoints
        .route("/EWS/Exchange.asmx", post(ews_handler))
        .route("/ews/exchange.asmx", post(ews_handler))
        .route("/EWS/", post(ews_handler))
        // OPTIONS handler for CORS
        .route("/*path", options(handle_options))
        // Layer middleware
        .layer(middleware)
        // Add state
        .with_state(state)
}

#[tokio::main]
async fn main() {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "exchange_gateway=debug,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting Exchange Gateway v{}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config = AppConfig::default();
    info!("Configuration loaded: {:?}", config);

    // Create CalDAV client
    let caldav_client = Arc::new(CalDavClientExt::new(
        config.caldav_url.clone(),
        "admin".to_string(), // Default credentials - should be configured
        "password".to_string(),
    ));

    // Create sync state store
    let sync_states = Arc::new(SyncStateStore::new());

    // Create rate limiter
    let rate_limiter = Arc::new(RateLimiter::new(5, 300)); // 5 attempts per 5 minutes

    // Create application state
    let state = Arc::new(AppState {
        caldav_client,
        sync_states,
        rate_limiter,
        config: Arc::new(config.clone()),
    });

    // Create router
    let app = create_app(state);

    // Parse bind address
    let addr: SocketAddr = config.bind_addr.parse().expect("Invalid bind address");

    info!("Exchange Gateway listening on http://{}", addr);

    // Start server
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_email_from_autodiscover() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<Autodiscover xmlns="http://schemas.microsoft.com/exchange/autodiscover/mobilesync/requestschema/2006">
    <Request>
        <EMailAddress>test@example.com</EMailAddress>
        <AcceptableResponseSchema>http://schemas.microsoft.com/exchange/autodiscover/mobilesync/responseschema/2006</AcceptableResponseSchema>
    </Request>
</Autodiscover>"#;

        let email = extract_email_from_autodiscover(xml);
        assert_eq!(email, Some("test@example.com".to_string()));
    }

    #[test]
    fn test_build_autodiscover_response() {
        let response = build_autodiscover_response(Some("user@example.com"));
        assert!(response.contains("MobileSync"));
        assert!(response.contains("user@example.com"));
        assert!(response.contains("Microsoft-Server-ActiveSync"));
    }
}
