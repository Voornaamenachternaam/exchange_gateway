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

pub async fn handle(
    State(config): State<Config>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {

    if let Some(auth) = headers.get("authorization") {
        if let Ok(auth_str) = auth.to_str() {
            if let Some(b64) = auth_str.strip_prefix("Basic ") {
                if let Ok(decoded) = STANDARD.decode(b64) {
                    if let Ok(creds) = String::from_utf8(decoded) {
                        if !authenticate(&config, &creds).await {
                            return unauthorized();
                        }
                    }
                }
            }
        }
    }

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

async fn authenticate(config: &Config, creds: &str) -> bool {
    config.validate_credentials(creds)
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

fn escape_xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
