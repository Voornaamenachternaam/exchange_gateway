// src/oab.rs
use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::models::AppState;

/// Zero-copy representation of a minimal gzip header (10 bytes).
/// Used to construct a valid empty gzip stream for OAB responses.
#[derive(Debug, Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C, packed)]
struct GzipHeader {
    id1: u8,
    id2: u8,
    cm: u8,
    flg: u8,
    mtime: [u8; 4],
    xfl: u8,
    os: u8,
}

const EMPTY_GZIP_BODY: [u8; 10] = [
    0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

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
    let header = GzipHeader {
        id1: 0x1F,
        id2: 0x8B,
        cm: 0x08,
        flg: 0x00,
        mtime: [0x00, 0x00, 0x00, 0x00],
        xfl: 0x00,
        os: 0xFF,
    };
    let mut data = Vec::with_capacity(header.as_bytes().len() + EMPTY_GZIP_BODY.len());
    data.extend_from_slice(header.as_bytes());
    data.extend_from_slice(&EMPTY_GZIP_BODY);
    data
}
