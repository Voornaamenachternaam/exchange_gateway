use axum::{
    body::Body,
    http::StatusCode,
    response::{IntoResponse, Response},
};

pub async fn health_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(Body::from(r#"{"status":"healthy"}"#))
        .unwrap()
}

pub async fn status_handler() -> impl IntoResponse {
    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "text/plain")
        .body(Body::from("Exchange Gateway OK"))
        .unwrap()
}
