// src/main.rs
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use exchange_gateway::{
    autodiscover, config::Config, eas, ecp, ews, logging, metrics::REGISTRY, metrics::record_http,
    models::AppState, oab, rate_limit::check_rate_limit, storage::Storage, util,
    validation::validate_request,
};
use prometheus::{Encoder, TextEncoder};
use tokio::net::TcpListener;
use tokio::signal;
use tower_http::{
    compression::CompressionLayer, limit::RequestBodyLimitLayer,
    sensitive_headers::SetSensitiveRequestHeadersLayer, set_header::SetResponseHeaderLayer,
    timeout::RequestBodyTimeoutLayer, trace::TraceLayer,
};
use tracing::{debug, info, warn};

const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 60;

/// Build the authentication advertisement the Autodiscover Outlook response
/// renders in its EXCH/EXPR `<Protocol>` blocks. When MAPI/HTTP HMA is
/// configured (`GATEWAY_MAPI_HMA_ENABLED` with a populated
/// `GATEWAY_MAPI_OIDC_ISSUER`) the gateway advertises Modern Auth
/// (`OAuth2/CertificateBased` + `<OauthUrl>` + `<CompactDomain>`) so New
/// Outlook for Windows provisions the account via Modern Auth; otherwise it
/// advertises Basic auth (backwards-compatible).
fn autodiscover_auth_advert(cfg: &Config) -> autodiscover::AuthAdvert {
    if cfg.mapi_hma_enabled && !cfg.mapi_oidc_issuer.is_empty() {
        autodiscover::AuthAdvert::Modern {
            oauth_url: cfg.mapi_oidc_issuer.clone(),
        }
    } else {
        autodiscover::AuthAdvert::Basic
    }
}

async fn autodiscover_xml(
    State(state): State<Arc<AppState>>,
    method: axum::http::Method,
    headers: axum::http::HeaderMap,
    Query(params): Query<HashMap<String, String>>,
    body: String,
) -> Response {
    use secrecy::ExposeSecret;
    let start = std::time::Instant::now();
    let host = &state.cfg.gateway_host;

    // For GET requests (redirect discovery per MS-OXDISCO §3.1.5.4),
    // use email from query parameter or fall back to empty string.
    // For POST requests, parse email from the XML body.
    let email = if method == axum::http::Method::GET {
        params
            .iter()
            .find(|(k, _)| {
                k.eq_ignore_ascii_case("emailaddress") || k.eq_ignore_ascii_case("email")
            })
            .map(|(_, v)| exchange_gateway::util::nfc(v.trim()))
            .unwrap_or_default()
    } else {
        autodiscover::extract_email_from_body_xml(&body).unwrap_or_default()
    };

    // Extract Accept-Language header for culture in mobilesync response
    let accept_language = headers
        .get(axum::http::header::ACCEPT_LANGUAGE)
        .and_then(|v| v.to_str().ok());

    debug!(
        target: "http",
        method = %method,
        path = "/autodiscover/autodiscover.xml",
        body_len = body.len(),
        email = %util::redact_email(&email),
        "Autodiscover XML request received"
    );

    // Resolve the mobilesync <User>/<DisplayName> (MS-ASCMD §2.2.3.49.1) for
    // ActiveSync / Outlook Android provisioning. Per MS-ASCMD the element is
    // "the user's display name in the directory service", so when a directory
    // service is configured we resolve the account's real name; otherwise we
    // fall back to a name derived from the request email (never the gateway
    // product brand, which would be presented as the account owner's
    // identity). Three gating rules avoid wasted/unsafe work:
    //
    //  1. Schema: `mobilesync_display_name` is consumed ONLY by the mobilesync
    //     response branch; Outlook-desktop requests never read it. Detect the
    //     schema first and skip the whole resolution for Outlook requests.
    //  2. Blocking work: `derive_display_name` (the no-directory fallback) is
    //     pure string manipulation — never offload it to `spawn_blocking`.
    //     Only the directory `resolve_email_blocking` path needs a blocking
    //     thread (the trait contract is blocking).
    //  3. Security: a directory lookup for an arbitrary client-supplied email
    //     discloses that account's real display name (PII) and enables
    //     directory-name enumeration by anonymous callers or by callers who
    //     supply *another* user's email. So the directory is only consulted
    //     when the request carries Basic credentials that authenticate against
    //     Stalwart AND the authenticated principal's canonical email matches
    //     the requested email. Anonymous or mismatched callers get only the
    //     disclosure-free `derive_display_name` fallback (built solely from
    //     the email they themselves supplied).
    let display_name = if !autodiscover::is_mobilesync_schema(&body) {
        // Outlook-desktop path does not use mobilesync_display_name at all.
        String::new()
    } else if state.directory.is_none() {
        // No directory configured → pure derivation, no blocking thread.
        autodiscover::derive_display_name(&email)
    } else {
        // Directory present → only let it disclose a name to the account's
        // own authenticated owner. Decode Basic creds, then short-circuit on
        // the principal match BEFORE the backend auth round-trip (so a caller
        // supplying *another* user's email never triggers a Stalwart
        // verifyCredentials call — a DoS-amplification guard). Verify the
        // CANONICAL username (matching the gateway's other authenticated
        // paths), and hold the password in a zeroized `SecretString` for the
        // lifetime of this check rather than a bare `String`.
        let dir_eligible = match decode_basic_auth(&headers) {
            Some((user, pass)) => {
                let auth_user = util::canonicalize_username(&user, &state.cfg.mail_domain);
                let request_canonical =
                    util::canonicalize_username(&email, &state.cfg.mail_domain);
                let secret_pass = secrecy::SecretString::from(pass);
                !auth_user.is_empty()
                    && auth_user == request_canonical
                    && state
                        .auth_verifier
                        .verify(&auth_user, secret_pass.expose_secret())
                        .await
            }
            None => false,
        };
        if dir_eligible {
            let email_for_resolve = email.clone();
            let dir = state.directory.clone();
            let handle = tokio::task::spawn_blocking(move || {
                autodiscover::resolve_user_display_name(dir.as_ref(), &email_for_resolve)
            });
            match handle.await {
                Ok(name) => name,
                Err(err) => {
                    // JoinError (panic / cancellation / pool exhaustion) —
                    // never silently drop it. Log with the redacted email so
                    // runtime instability is diagnosable, then fall back to
                    // the disclosure-free derive so the response still carries
                    // a name instead of being silently emptied.
                    warn!(
                        target: "http",
                        email = %util::redact_email(&email),
                        error = %err,
                        "spawn_blocking display-name resolution failed; \
                         falling back to derived name"
                    );
                    autodiscover::derive_display_name(&email)
                }
            }
        } else {
            // Anonymous / unauthenticated / principal mismatch → do NOT
            // consult the directory; derive solely from the supplied email.
            autodiscover::derive_display_name(&email)
        }
    };

    let auth_advert = autodiscover_auth_advert(&state.cfg);
    let req = autodiscover::AutodiscoverXmlRequest {
        host: host.as_str(),
        body: &body,
        email: &email,
        accept_language,
        mail_host: &state.cfg.mail_host,
        include_imap_smtp: state.smtp_client.is_some(),
        auth_advert: &auth_advert,
        mobilesync_display_name: &display_name,
    };
    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_xml(&req);

    let elapsed_ms = start.elapsed().as_millis();
    let success = status.is_success();

    if success {
        info!(
            target: "http",
            method = %method,
            path = "/autodiscover/autodiscover.xml",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            response_len = body_out.len(),
            email = %util::redact_email(&email),
            "Autodiscover XML completed"
        );
    } else {
        warn!(
            target: "http",
            method = %method,
            path = "/autodiscover/autodiscover.xml",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            email = %util::redact_email(&email),
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
        email = ?params.email.as_deref().map(util::redact_email),
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

/// Autodiscover V2 JSON handler with email in URL path.
///
/// Some Outlook versions use the path format
/// `/autodiscover/autodiscover.json/v1.0/{email}` instead of query parameters.
/// This handler extracts the email from the path and the protocol from
/// query parameters, then delegates to the standard JSON handler.
async fn autodiscover_json_v1(
    State(state): State<Arc<AppState>>,
    Path(email): Path<String>,
    Query(params): Query<autodiscover::AutodiscoverJsonParams>,
) -> Response {
    let start = std::time::Instant::now();
    let host = &state.cfg.gateway_host;

    debug!(
        target: "http",
        method = "GET",
        path = "/autodiscover/autodiscover.json/v1.0/{email}",
        protocol = ?params.protocol,
        email = %util::redact_email(&email),
        "Autodiscover JSON V2 path request received"
    );

    let (status, hdrs, body_out) = autodiscover::handle_autodiscover_json(
        host,
        params.protocol.as_deref(),
        Some(email.as_str()),
    );

    let elapsed_ms = start.elapsed().as_millis();
    let success = status.is_success();

    if success {
        info!(
            target: "http",
            method = "GET",
            path = "/autodiscover/autodiscover.json/v1.0/{email}",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            response_len = body_out.len(),
            protocol = ?params.protocol,
            "Autodiscover JSON V2 path completed"
        );
    } else {
        warn!(
            target: "http",
            method = "GET",
            path = "/autodiscover/autodiscover.json/v1.0/{email}",
            status = status.as_u16(),
            elapsed_ms = elapsed_ms,
            protocol = ?params.protocol,
            "Autodiscover JSON V2 path failed"
        );
    }

    build_response(status, &hdrs, body_out)
}

/// Offline Address Book (OAB) download handler — MS-OXOAB / MS-OXWOAB.
///
/// Serves the OAB virtual directory advertised by `autodiscover::oab_url` so
/// Outlook for Windows can download a directory-backed offline address book
/// instead of hitting a 404 (audit gap §1.1). Delegates to [`oab::handle_oab`]
/// which authenticates via Basic auth against the shared [`AuthVerifier`],
/// serves the `oab.xml` manifest and a real OAB v3 details binary generated
/// from the operator-configured directory. See `src/oab.rs` for the format
/// and security details.
async fn oab_download(
    State(state): State<Arc<AppState>>,
    Path((guid, file)): Path<(String, String)>,
    headers: axum::http::HeaderMap,
) -> Response {
    oab::handle_oab(State(state), guid, file, headers).await
}

/// ECP (Exchange Control Panel) landing page (no trailing path). Delegates to
/// `ecp::handle_ecp` with `path = None` so the bare `/ecp` and `/ecp/` routes
/// render the same authenticated landing page as a deep-linked path.
async fn ecp_root(
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> Response {
    ecp::handle_ecp(State(state), None, headers).await
}

/// ECP deep-link handler: New Outlook appends path segments + query strings
/// to the advertised `<EcpUrl>` base, producing requests like `/ecp/Options/`
/// or `/ecp/?rfr=ool&exsc=1`. The trailing-path capture is opaque to the
/// gateway; it is recorded as page context so the panel never 404s.
async fn ecp_path(
    State(state): State<Arc<AppState>>,
    Path(path): Path<String>,
    headers: axum::http::HeaderMap,
) -> Response {
    ecp::handle_ecp(State(state), Some(path), headers).await
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

/// MAPI/HTTP (MS-OXCMAPIHTTP) route handler. Two endpoints share this
/// handler: `/mapi/emsmdb` (mailbox RPCs) and `/mapi/nspi` (address-book
/// RPCs). When `GATEWAY_MAPI_ENABLED=false`, `AppState.mapi` is `None` and
/// the handler returns 404 so the surface is invisible unless opted in.
///
/// The `endpoint` argument identifies which physical path the request
/// landed on so that mailbox ROPs (Connect/Execute/Disconnect) sent to
/// `/mapi/nspi`, or address-book RPCs (Bind/QueryRows/ResolveNames) sent to
/// `/mapi/emsmdb`, are rejected with `InvalidRequestType` (code 5) rather
/// than being silently processed against the wrong RPC family — per
/// MS-OXCMAPIHTTP §2.2.5 the emsmdb endpoint serves the mailbox RPC set and
/// the nspi endpoint serves the NSPI RPC set.
///
/// Phase 0 wires the transport parse → orchestrator → render pipeline.
/// The orchestrator (`mapi::handler::handle`) currently implements
/// Connect/Disconnect/Execute-skeleton; deeper ROPs land in Phase 1.
async fn mapi_http_path(
    endpoint: &'static str,
    State(state): State<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let start = std::time::Instant::now();
    let mapi_state = match state.mapi.as_ref() {
        Some(s) => s.clone(),
        None => {
            // Endpoint not enabled — return 404 so the route is invisible
            // to clients that did not receive a MAPI <Protocol> block from
            // Autodiscover.
            return StatusCode::NOT_FOUND.into_response();
        }
    };

    let enabled = state.cfg.mapi_enabled;
    // Decode both the Basic-auth username and password once so the address-book
    // (NSPI) dispatcher can authenticate `/mapi/nspi` requests against the
    // same `AuthVerifier` the mailbox path uses, and so it can synthesise the
    // caller's own mailbox entry in a directory-less minimal-GAL stub. The
    // mailbox path only consumes the password; the username is read off the
    // same shared decoder (`decode_basic_auth`) so the credential shape stays
    // identical across every endpoint.
    let (basic_username, basic_password) = decode_basic_auth(&headers)
        .map(|(u, p)| (Some(u), Some(p)))
        .unwrap_or((None, None));
    let mut req =
        match exchange_gateway::mapi::transport::parse_request(&headers, body.to_vec(), enabled) {
            Ok(r) => r,
            Err(hdr_err) => {
                let code = hdr_err.to_response_code();
                let request_id = headers
                    .get("x-requestid")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                let resp = exchange_gateway::mapi::transport::MapiResponse::error(code, request_id);
                let (status, hdrs_out, ct, body_out) = resp.render();
                info!(
                    target: "http",
                    path = "/mapi/...",
                    response_code = code.as_u8(),
                    elapsed_ms = start.elapsed().as_millis(),
                    "MAPI/HTTP transport header-reject"
                );
                return render_mapi(status, hdrs_out, ct, body_out);
            }
        };
    req.password = basic_password;
    req.username = basic_username;

    // Reject RPC-family/endpoint mismatches (MS-OXCMAPIHTTP §2.2.5):
    // mailbox RPCs (Connect/Execute/Disconnect/NotificationPoll +
    // already-recognised address-book verbs) must hit `/mapi/emsmdb`, and
    // NSPI RPCs must hit `/mapi/nspi`. Outlook never sends mailbox RPCs to
    // `/mapi/nspi` but a misconfigured proxy or attacker might; guard here
    // with the transport-layer `InvalidRequestType` (code 5).
    if let Err(resp) = check_endpoint_rpc_family(endpoint, &req) {
        let response_code = resp.code.as_u8();
        let (status, hdrs_out, ct, body_out) = resp.render();
        info!(
            target: "http",
            path = endpoint,
            response_code,
            elapsed_ms = start.elapsed().as_millis(),
            "MAPI/HTTP RPC-family mismatch"
        );
        return render_mapi(status, hdrs_out, ct, body_out);
    }

    let resp = exchange_gateway::mapi::handler::handle(req, &mapi_state).await;
    let response_code = resp.code.as_u8();
    let (status, hdrs_out, ct, body_out) = resp.render();
    info!(
        target: "http",
        path = "/mapi/...",
        response_code = response_code,
        elapsed_ms = start.elapsed().as_millis(),
        "MAPI/HTTP request completed"
    );
    render_mapi(status, hdrs_out, ct, body_out)
}

/// Render a MAPI/HTTP response tuple (status, headers, content-type, body)
/// into an axum `Response`. The content-type is `application/mapi-http` per
/// MS-OXCMAPIHTTP §2.2.3.2.2.
fn render_mapi(
    status: StatusCode,
    hdrs: axum::http::HeaderMap,
    content_type: &'static str,
    body: Vec<u8>,
) -> Response {
    let mut resp = (status, body).into_response();
    resp.headers_mut().extend(hdrs);
    if let Ok(ct) = HeaderValue::from_str(content_type) {
        resp.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    resp
}

/// Shared decoder for an `Authorization: Basic <b64>` header that returns BOTH
/// the username (verbatim, up to the first `:`) and the password. This is the
/// single source of truth for the Basic-credential decode shape (scheme
/// stripping, base64 STANDARD, UTF-8, `user:pass` split — RFC 7617) so the
/// MAPI mailbox path, the MAPI address-book (NSPI) path, and the Autodiscover
/// auth-gating path cannot drift apart in malformed-header handling. Returns
/// `None` for absent, malformed, non-Basic, invalid base64/UTF-8, or a missing
/// password separator. The password is returned as a **plain** `String`;
/// callers that need defense-in-depth MUST wrap it in
/// `secrecy::SecretString` (zeroized on drop) — the established pattern in
/// `auth.rs` / `mapi/handler.rs`.
fn decode_basic_auth(headers: &axum::http::HeaderMap) -> Option<(String, String)> {
    use base64::Engine;
    let raw = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let rest = strip_auth_scheme_basic(raw)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(rest)
        .ok()?;
    let plain = String::from_utf8(decoded).ok()?;
    let (user, pass) = plain.split_once(':')?;
    if pass.is_empty() {
        None
    } else {
        Some((user.to_string(), pass.to_string()))
    }
}

/// Strip a case-insensitive `Basic ` auth-scheme prefix from the
/// `Authorization` header value, returning the credential remainder.
fn strip_auth_scheme_basic(raw: &str) -> Option<&str> {
    let scheme_end = raw.find([' ', '\t']).filter(|&i| i > 0)?;
    let (scheme, rest) = raw.split_at(scheme_end);
    if !scheme.eq_ignore_ascii_case("Basic") {
        return None;
    }
    // Skip the single separating SP/HT; treat the rest as the credential.
    let rest = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
    Some(rest)
}

/// Reject RPC-family/endpoint mismatches (MS-OXCMAPIHTTP §2.2.5): mailbox
/// ROP verbs (Connect/Execute/Disconnect/NotificationWait/PING) only belong
/// on `/mapi/emsmdb`, and address-book RPCs only belong on `/mapi/nspi`.
/// Returns `Err(MapiResponse)` with `InvalidRequestType` (code 5) when a
/// verb is sent to the wrong endpoint, so the dispatcher never processes a
/// mailbox ROP against the address-book surface or vice-versa.
fn check_endpoint_rpc_family(
    endpoint: &str,
    req: &exchange_gateway::mapi::transport::MapiRequest,
) -> Result<(), exchange_gateway::mapi::transport::MapiResponse> {
    use exchange_gateway::mapi::transport::{MapiResponse, ResponseCode, RpcKind};
    let is_emsmdb = endpoint == "/mapi/emsmdb";
    match req.kind {
        RpcKind::Mailbox(_) => {
            if is_emsmdb {
                Ok(())
            } else {
                Err(MapiResponse::error(
                    ResponseCode::InvalidRequestType,
                    req.request_id.clone(),
                ))
            }
        }
        RpcKind::AddressBook(_) => {
            if is_emsmdb {
                Err(MapiResponse::error(
                    ResponseCode::InvalidRequestType,
                    req.request_id.clone(),
                ))
            } else {
                Ok(())
            }
        }
    }
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

    // Install the process-wide advertised Exchange server version (already
    // validated in `Config::load`). All EWS/Autodiscover version stamps render
    // from this single source of truth (`version::current()`).
    let server_version = config.server_version_info()?;
    exchange_gateway::version::init(server_version.clone());
    tracing::info!(
        "Advertising Exchange server version {} ({}) — product: {}",
        server_version.version_string(),
        server_version.exchange_version(),
        exchange_gateway::version::PRODUCT_NAME,
    );

    tracing::info!(
        "Exchange Gateway starting. bind={} gateway_host={}",
        config.bind,
        config.gateway_host,
    );

    let storage =
        Arc::new(Storage::new(&format!("sqlite://{}?mode=rwc", config.database_path)).await?);
    storage.init_schema().await?;

    let app_state = Arc::new(AppState::new(config.clone(), storage));

    // Idle-session sweeper (#3593666961): if MAPI/HTTP is enabled, run
    // `SessionManager::sweep_idle` at the configured/idle-TTL cadence so
    // abandoned Connect+Execute sessions (e.g. a soft-kill of the Outlook
    // client, lost laptop, etc.) actually expire rather than leaking
    // forever. Falls out of scope when `mapi` is `None` (disabled).
    if let Some(mapi_state) = app_state.mapi.as_ref() {
        let sessions = mapi_state.sessions.clone();
        let idle_secs = exchange_gateway::mapi::session::SessionManager::default_idle_secs();
        tokio::spawn(async move {
            // Run at the same cadence as the idle TTL so a sweep lands
            // roughly one TTL after a session goes quiet.
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(idle_secs));
            interval.tick().await; // first immediate tick - skip.
            loop {
                interval.tick().await;
                let removed = sessions.sweep_idle();
                if removed > 0 {
                    debug!(target: "mapi", removed, "idle sessions swept");
                }
            }
        });
    }

    let app = Router::new()
        .route("/health", get(health_check))
        .route("/metrics", get(metrics_handler))
        .route(
            "/EWS/Exchange.asmx",
            post(ews::handle).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route(
            "/EWS/{*path}",
            post(ews::handle).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route(
            "/Microsoft-Server-ActiveSync",
            any(eas::handle).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        // MAPI/HTTP advertises up to 128 MiB per request (MS-OXCMAPIHTTP);
        // apply the larger body limit per-route, not globally, so the smaller
        // 4 MiB cap on EWS/EAS/Autodiscover does not silently reject large
        // ROP batches before `mapi::transport::parse_request` ever runs. The
        // transport layer's own `MAX_MAPI_BODY_BYTES` check is the
        // authoritative envelope bound.
        .route(
            "/mapi/emsmdb",
            post(|st, h, b| mapi_http_path("/mapi/emsmdb", st, h, b)).layer(
                RequestBodyLimitLayer::new(exchange_gateway::mapi::transport::MAX_MAPI_BODY_BYTES),
            ),
        )
        .route(
            "/mapi/nspi",
            post(|st, h, b| mapi_http_path("/mapi/nspi", st, h, b)).layer(
                RequestBodyLimitLayer::new(exchange_gateway::mapi::transport::MAX_MAPI_BODY_BYTES),
            ),
        )
        .route(
            "/autodiscover/autodiscover.xml",
            any(autodiscover_xml).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route(
            "/Autodiscover/Autodiscover.xml",
            any(autodiscover_xml).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route(
            "/autodiscover/autodiscover.svc",
            post(autodiscover_soap).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route(
            "/Autodiscover/Autodiscover.svc",
            post(autodiscover_soap).layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES)),
        )
        .route("/autodiscover/autodiscover.json", get(autodiscover_json))
        .route("/Autodiscover/autodiscover.json", get(autodiscover_json))
        .route(
            "/autodiscover/autodiscover.json/v1.0/{email}",
            get(autodiscover_json_v1),
        )
        .route(
            "/Autodiscover/autodiscover.json/v1.0/{email}",
            get(autodiscover_json_v1),
        )
        // Offline Address Book (MS-OXOAB / MS-OXWOAB) download endpoint.
        // Serves the OAB virtual directory advertised by Autodiscover under
        // `/OAB/{OAB_SERVER_GUID}/` — both the `oab.xml` manifest and the
        // generated OAB v3 details binary land on this single route since the
        // file name is the trailing path segment. The body limit matches the
        // cap used by MAPI/HTTP so a large GAL serialisation is never clipped
        // by the route itself.
        .route("/OAB/{guid}/{file}", get(oab_download))
        // Exchange Control Panel (ECP) settings surface — closes audit gap
        // §1.3 ("No `<EcpUrl>` real value`"). Autodiscover advertises
        // `<EcpUrl>{gateway}/ecp/</EcpUrl>` (see `autodiscover::ecp_url`) and
        // `ecp::handle_ecp` serves the backing virtual directory so the
        // Out-of-Office / OptIn / Regional deep-links Outlook constructs by
        // appending to that base resolve to a real authenticated page
        // instead of the EWS SOAP endpoint. The body limit matches the
        // other small-payload endpoints; the page is static HTML.
        .route("/ecp", get(ecp_root))
        .route("/ecp/", get(ecp_root))
        .route("/ecp/{*path}", get(ecp_path))
        .with_state(app_state.clone())
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            record_http,
        ))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            validate_request,
        ))
        .layer(middleware::from_fn_with_state(
            app_state.clone(),
            check_rate_limit,
        ))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
        ]))
        .layer(TraceLayer::new_for_http())
        .layer(RequestBodyTimeoutLayer::new(Duration::from_secs(
            REQUEST_TIMEOUT_SECS,
        )))
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
        ));

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
        jmap_configured = !state.cfg.jmap_base.is_empty(),
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
        return (StatusCode::SERVICE_UNAVAILABLE, "Database unavailable").into_response();
    }

    // Optionally check JMAP and/or CalDAV backend health.
    // JMAP is preferred (single endpoint for email + calendar).
    // CalDAV is checked as a fallback when JMAP is not configured.
    let jmap_configured = !state.cfg.jmap_base.is_empty();
    let caldav_configured = !state.cfg.caldav_base.is_empty();

    if jmap_configured {
        match verify_jmap_health(&state).await {
            Ok(_) => {
                let elapsed_ms = start.elapsed().as_millis();
                info!(
                    target: "health",
                    status = "healthy",
                    check = "jmap",
                    elapsed_ms = elapsed_ms,
                    "Health check passed (JMAP)"
                );
                (StatusCode::OK, "OK").into_response()
            }
            Err(e) => {
                // JMAP failed — try CalDAV as fallback
                if caldav_configured {
                    match verify_caldav_health(&state).await {
                        Ok(_) => {
                            let elapsed_ms = start.elapsed().as_millis();
                            warn!(
                                target: "health",
                                status = "degraded",
                                jmap_error = %e,
                                elapsed_ms = elapsed_ms,
                                "JMAP unhealthy but CalDAV OK — degraded mode"
                            );
                            (StatusCode::OK, "OK (degraded: JMAP unavailable)").into_response()
                        }
                        Err(caldav_err) => {
                            let elapsed_ms = start.elapsed().as_millis();
                            warn!(
                                target: "health",
                                status = "unhealthy",
                                jmap_error = %e,
                                caldav_error = %caldav_err,
                                elapsed_ms = elapsed_ms,
                                "Both JMAP and CalDAV backends unavailable"
                            );
                            (StatusCode::SERVICE_UNAVAILABLE, "Backends unavailable")
                                .into_response()
                        }
                    }
                } else {
                    let elapsed_ms = start.elapsed().as_millis();
                    warn!(
                        target: "health",
                        status = "unhealthy",
                        check = "jmap",
                        elapsed_ms = elapsed_ms,
                        error = %e,
                        "Health check failed - JMAP backend unavailable"
                    );
                    (StatusCode::SERVICE_UNAVAILABLE, "JMAP backend unavailable").into_response()
                }
            }
        }
    } else if caldav_configured {
        match verify_caldav_health(&state).await {
            Ok(_) => {
                let elapsed_ms = start.elapsed().as_millis();
                info!(
                    target: "health",
                    status = "healthy",
                    check = "caldav",
                    elapsed_ms = elapsed_ms,
                    "Health check passed (CalDAV)"
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
                    "CalDAV backend unavailable",
                )
                    .into_response()
            }
        }
    } else {
        let elapsed_ms = start.elapsed().as_millis();
        info!(
            target: "health",
            status = "healthy",
            elapsed_ms = elapsed_ms,
            "Health check passed (no backend configured)"
        );
        (StatusCode::OK, "OK").into_response()
    }
}

/// Prometheus metrics endpoint.
async fn metrics_handler() -> impl IntoResponse {
    let encoder = TextEncoder::new();
    let metric_families = REGISTRY.gather();
    let mut buffer = Vec::new();
    if encoder.encode(&metric_families, &mut buffer).is_err() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Failed to encode metrics",
        )
            .into_response();
    }
    let metrics_text =
        String::from_utf8(buffer).unwrap_or_else(|_| "Failed to decode metrics".to_string());
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics_text,
    )
        .into_response()
}

async fn verify_jmap_health(state: &Arc<AppState>) -> Result<()> {
    use exchange_gateway::jmap::JmapClient;
    let jmap = JmapClient::new(&state.cfg.jmap_base)?;
    jmap.health_check().await
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
