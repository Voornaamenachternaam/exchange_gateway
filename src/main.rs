// src/main.rs
use axum::{
    Router,
    routing::{any, post},
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod caldav;
mod calendar;
mod config;
mod eas;
mod ews;
mod models;
mod storage;
mod sync;
mod wbxml;

use crate::config::Config;
use crate::models::AppState;
use crate::storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging early, but use explicit print for critical startup errors
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load config from the absolute path used in Dockerfile/Compose
    let config = match Config::load("/etc/exchange-gateway/config.toml") {
        Ok(c) => c,
        Err(e) => {
            eprintln!("CRITICAL: Failed to load config: {}", e);
            return Err(e);
        }
    };

    tracing::info!("Configuration loaded successfully. Initializing storage...");

    let storage = Arc::new(Storage::new(&config.worker_url, &config.worker_secret)?);
    let app_state = Arc::new(AppState {
        cfg: config.clone(),
        storage: storage.clone(),
    });

    let app = Router::new()
        .route("/EWS/*path", post(ews::handle))
        .route("/EWS/Exchange.asmx", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("Exchange Gateway listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
