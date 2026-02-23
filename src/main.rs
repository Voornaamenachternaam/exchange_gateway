// src/main.rs
use axum::{
    routing::{post, any},
    Router,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;
use std::sync::Arc;

mod caldav;
mod config;
mod eas;
mod ews;
mod ews_marshaller;
mod models;
mod rrule_engine;
mod storage;
mod sync;
mod utils;
mod wbxml;

use config::Config;
use crate::models::AppState;
use crate::storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load gateway configuration
    let config = Config::load("config.toml")?;
    // Initialize storage with Cloudflare Worker endpoint
    let storage = Arc::new(Storage::new(&config.worker_url, &config.worker_secret)?);
    let app_state = Arc::new(AppState {
        cfg: config.clone(),
        storage: storage.clone(),
    });

    // Build router for EWS and ActiveSync endpoints
    let app = Router::new()
        .route("/EWS/{*path}", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state);

    // Bind to configured address (e.g., 0.0.0.0:8133)
    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;

    // Serve the application
    axum::serve(listener, app).await?;

    Ok(())
}
