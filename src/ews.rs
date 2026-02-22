// src/ews.rs
use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use crate::config::Config;

/// Handle Exchange Web Services (EWS) requests (minimal implementation).
pub async fn handle(
    State(config): State<Config>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    // Basic Auth: expect "Authorization: Basic <base64>"
    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(b64) = auth_str.trim().strip_prefix("Basic ") {
                let mut decoded = Vec::new();
                if STANDARD.decode_vec(b64.as_bytes(), &mut decoded).is_err() {
                    return unauthorized();
                }
                if String::from_utf8(decoded).is_err() {
                    return unauthorized();
                }
            } else {
                return unauthorized();
            }
        } else {
            return unauthorized();
        }
    } else {
        return unauthorized();
    }

    // For now, ignore SOAP content and always return a simple OK response
    let mut reader = Reader::from_str(&body);
    reader.config_mut().trim_text(true);

    let mut buf = Vec::new();
    while let Ok(event) = reader.read_event_into(&mut buf) {
        match event {
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    soap_response()
}

fn unauthorized() -> impl IntoResponse {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        "Unauthorized",
    )
}

fn soap_response() -> impl IntoResponse {
    (
        axum::http::StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        r#"<?xml version="1.0" encoding="utf-8"?><Response>OK</Response>"#,
    )
}
