// src/main.rs
use axum::{
    Router,
    routing::{any, post, get},
    response::Json,
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
use serde_json::json;

// Health endpoint handler
async fn health() -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "service": "exchange_gateway",
        "version": "1.0.22"
    }))
}

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

    // Build the router with all routes
    // EWS routes - multiple paths to handle different Outlook clients
    // /EWS/Exchange.asmx is the primary EWS endpoint
    // /EWS/*path handles any EWS sub-paths (for WSDL, etc.)
    let app = Router::new()
        .route("/health", get(health))
        .route("/EWS/Exchange.asmx", post(ews::handle))
        .route("/EWS/Services.wsdl", get(ews::handle_wsdl))
        .route("/EWS/*path", post(ews::handle))
        // ActiveSync endpoint - handles all ActiveSync commands (Sync, FolderSync, Ping, etc.)
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;
    let listener = TcpListener::bind(addr).await?;

    tracing::info!("Exchange Gateway listening on {}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}
