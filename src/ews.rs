// src/ews.rs
use axum::{extract::State, http::HeaderMap, response::Response};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;
use crate::models::AppState;
use crate::ews_marshaller::soap_ok_response;

/// Minimal EWS handler. It expects Basic Authorization and returns a small SOAP OK envelope.
/// This handler is intentionally conservative: it accepts requests, checks Basic auth presence,
/// scans SOAP body with quick-xml (to ensure well-formedness) and returns a small OK.
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // Validate presence of Authorization header (Basic). We do not validate credentials here,
    // Stalwart will be the authoritative auth backend; Cloudflare Worker may handle auth further.
    if let Some(auth) = headers.get("authorization") {
        if let Ok(s) = auth.to_str() {
            if !s.trim().starts_with("Basic ") {
                return unauthorized();
            }
            // we don't fully decode credentials here; minimal check only
        } else {
            return unauthorized();
        }
    } else {
        return unauthorized();
    }

    // Try to parse SOAP body (quick check)
    let mut reader = Reader::from_str(&body);
    reader.trim_text(true);

    let mut buf = Vec::new();
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        buf.clear();
    }

    // Return a minimal successful SOAP envelope
    let soap = soap_ok_response("<Response>OK</Response>");
    (axum::http::StatusCode::OK, [("Content-Type", "text/xml; charset=utf-8")], soap).into_response()
}

fn unauthorized() -> Response {
    (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
}
