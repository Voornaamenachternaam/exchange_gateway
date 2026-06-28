use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::Response,
};
use percent_encoding::percent_decode_str;
use std::sync::Arc;
use tracing::warn;

use crate::models::AppState;

const MAX_SYNC_KEY_LEN: usize = 512;
const MAX_COLLECTION_ID_LEN: usize = 64;
const MAX_ITEM_ID_LEN: usize = 256;

/// Middleware that validates incoming requests for basic security constraints.
pub async fn validate_request(
    State(state): State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path();
    let method = req.method();
    let cfg = &state.cfg;

    if let Some(len) = req.headers().get("content-length")
        && let Ok(size_str) = len.to_str()
        && let Ok(size) = size_str.parse::<usize>()
        && size > cfg.max_body_bytes
    {
        warn!(
            target: "validation",
            method = %method,
            path = %path,
            size = size,
            limit = cfg.max_body_bytes,
            "Request body exceeds maximum allowed size"
        );
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }

    // Path traversal check: decode percent-encoded sequences and check for /../ or leading ../
    if let Ok(decoded_bytes) = percent_decode_str(path).decode_utf8() {
        let decoded = decoded_bytes.as_ref();
        if decoded.contains("/../") || decoded.starts_with("../") {
            warn!(
                target: "validation",
                path = %path,
                "Path contains traversal sequence"
            );
            return Err(StatusCode::BAD_REQUEST);
        }
    }

    Ok(next.run(req).await)
}

pub fn validate_sync_key(sync_key: &str) -> bool {
    !sync_key.is_empty() && sync_key.len() <= MAX_SYNC_KEY_LEN
}

pub fn validate_collection_id(collection_id: &str) -> bool {
    !collection_id.is_empty()
        && collection_id.len() <= MAX_COLLECTION_ID_LEN
        && collection_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ' ')
}

pub fn validate_item_id(item_id: &str) -> bool {
    !item_id.is_empty()
        && item_id.len() <= MAX_ITEM_ID_LEN
        && !item_id.contains('/')
        && !item_id.contains('\\')
        && !item_id.contains('\0')
        && item_id
            .chars()
            .all(|c| c.is_ascii_graphic() && !c.is_ascii_control())
}

pub fn validate_attachment_size(size: usize, max_size: usize) -> bool {
    size <= max_size
}

pub fn validate_attachment_id(att_id: &str) -> bool {
    validate_item_id(att_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_sync_key() {
        assert!(validate_sync_key("valid_sync_key_123"));
        assert!(validate_sync_key(&"a".repeat(512)));
        assert!(!validate_sync_key(&"a".repeat(513)));
        assert!(!validate_sync_key(""));
    }

    #[test]
    fn test_validate_collection_id() {
        assert!(validate_collection_id("2"));
        assert!(validate_collection_id("inbox_123"));
        assert!(validate_collection_id("Sent Items"));
        assert!(!validate_collection_id("invalid/col"));
        assert!(!validate_collection_id(&"a".repeat(65)));
    }

    #[test]
    fn test_validate_item_id() {
        assert!(validate_item_id("em-12345"));
        assert!(validate_item_id("item-uuid-here"));
        assert!(!validate_item_id("bad/path"));
        assert!(!validate_item_id("contains\x00null"));
    }

    #[test]
    fn test_validate_attachment_size() {
        assert!(validate_attachment_size(1024, 1024 * 1024));
        assert!(!validate_attachment_size(
            1024 * 1024 * 50,
            1024 * 1024 * 10
        ));
    }
}
