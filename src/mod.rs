use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::sync::Arc;

mod autodiscover;
mod ews;
mod health;
mod sync;

pub use autodiscover::autodiscover_handler;
pub use ews::ews_handler;
pub use health::{health_handler, status_handler};
pub use sync::sync_handler;

use crate::models::AppState;

pub async fn options_handler(headers: HeaderMap) -> impl IntoResponse {
    let origin = headers
        .get("origin")
        .map(|v| v.to_str().unwrap_or("*"))
        .unwrap_or("*");

    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Access-Control-Allow-Origin", origin)
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header(
            "Access-Control-Allow-Headers",
            "Content-Type, Authorization, X-Requested-With",
        )
        .header("Access-Control-Max-Age", "86400")
        .body(axum::body::Body::empty())
        .unwrap()
}

fn extract_basic_auth(headers: &HeaderMap) -> Option<(String, String)> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    if !auth.starts_with("Basic ") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&auth[6..])
        .ok()?;
    let creds = String::from_utf8(decoded).ok()?;
    let parts: Vec<&str> = creds.splitn(2, ':').collect();
    if parts.len() != 2 {
        return None;
    }
    Some((parts[0].to_string(), parts[1].to_string()))
}

fn extract_device_info(headers: &HeaderMap) -> (String, String) {
    let device_id = headers
        .get("x-ms-asdeviceid")
        .or_else(|| headers.get("X-MS-ASDeviceId"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();

    let device_type = headers
        .get("x-ms-asdevicetype")
        .or_else(|| headers.get("X-MS-ASDeviceType"))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("Unknown")
        .to_string();

    (device_id, device_type)
}

fn make_xml_response(xml: String) -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.ms-sync.wbxml")
        .header("Cache-Control", "private, no-store")
        .body(axum::body::Body::from(xml))
        .unwrap()
}

fn make_wbxml_response(bytes: Vec<u8>) -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/vnd.ms-sync.wbxml")
        .header("Cache-Control", "private, no-store")
        .body(axum::body::Body::from(bytes))
        .unwrap()
}

fn make_soap_response(xml: String) -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("Cache-Control", "private, no-store")
        .body(axum::body::Body::from(xml))
        .unwrap()
}

fn make_json_response(json: String) -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .header("Cache-Control", "private, no-store")
        .body(axum::body::Body::from(json))
        .unwrap()
}

fn unauthorized_response() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::UNAUTHORIZED)
        .header("WWW-Authenticate", "Basic realm=\"Exchange Gateway\"")
        .header("Content-Type", "text/plain")
        .body(axum::body::Body::from("Unauthorized"))
        .unwrap()
}
