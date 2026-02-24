// src/main.rs
use axum::{
    routing::{any, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod caldav;
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
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Config::load("config.toml")?;
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
