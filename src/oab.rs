// src/oab.rs
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::models::AppState;

pub async fn handle_oab_list(State(state): State<Arc<AppState>>) -> Response {
    let host_escaped = crate::util::xml_escape(&state.cfg.gateway_host);
    let oab_url = format!("https://{}/OAB/oab.xml", host_escaped);
    let name = "Default Offline Address Book";
    let xml = format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<OAB xmlns="http://schemas.microsoft.com/exchange/oab/2006/01">
  <OABUrl>
    <Url>{oab_url}</Url>
    <Name>{name}</Name>
    <IsDefault>true</IsDefault>
  </OABUrl>
</OAB>"#
    );

    (
        StatusCode::OK,
        [
            ("Content-Type", "application/xml; charset=utf-8"),
            ("X-CasHttpStatus", "0"),
        ],
        xml,
    )
        .into_response()
}

pub async fn handle_oab_file(State(_state): State<Arc<AppState>>) -> Response {
    let body = build_empty_oab_gzip();

    (
        StatusCode::OK,
        [("Content-Type", "application/octet-stream")],
        body,
    )
        .into_response()
}

fn build_empty_oab_gzip() -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(
        b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x00\xff\x03\x00\x00\x00\x00\x00\x00\x00\x00\x00",
    );
    data
}
