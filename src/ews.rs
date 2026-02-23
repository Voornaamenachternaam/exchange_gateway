// src/ews.rs
use axum::{
    extract::State,
    http::HeaderMap,
    response::IntoResponse,
};
use quick_xml::Reader;
use quick_xml::events::Event;
use std::sync::Arc;
use crate::caldav::CaldavClient;
use crate::eas::parse_basic_auth;
use crate::models::AppState;

/// Handle Exchange Web Services (EWS) requests (minimal implementation).
pub async fn handle(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: String,
) -> axum::response::Response {
    // Extract and validate credentials via the CalDAV backend
    let (username, password) = match parse_basic_auth(&headers) {
        Some(creds) => creds,
        None => return unauthorized(),
    };

    let caldav = CaldavClient::new(&state.cfg);
    if caldav.find_user_calendars(&username, &password).await.is_err() {
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

fn unauthorized() -> axum::response::Response {
    (
        axum::http::StatusCode::UNAUTHORIZED,
        "Unauthorized",
    ).into_response()
}

fn soap_response() -> axum::response::Response {
    (
        axum::http::StatusCode::OK,
        [("Content-Type", "text/xml; charset=utf-8")],
        r#"<?xml version="1.0" encoding="utf-8"?><Response>OK</Response>"#,
    ).into_response()
}
