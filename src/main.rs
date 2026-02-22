// src/main.rs
use axum::{
    routing::{any, post},
    Router,
};
use hyper::server;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

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
use models::AppState;
use storage::Storage;

/// Entry point: reads /etc/exchange-gateway/config.toml to configure the gateway.
/// This main.rs avoids relying on fragile re-exports by calling the hyper server via the
/// canonical `server::Server` type path (stable across hyper versions).
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load configuration
    let config = Config::load("/etc/exchange-gateway/config.toml")?;
    // Storage uses Cloudflare Worker endpoint and secret
    let storage = Storage::new(&config.worker_url, &config.worker_secret)?;
    let app_state = Arc::new(AppState { cfg: config.clone(), storage: Arc::new(storage) });

    // Build router
    let app = Router::new()
        .route("/EWS/*path", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state);

    let addr: SocketAddr = config.bind.parse()?;
    tracing::info!("Exchange gateway listening on http://{}", addr);

    // Use hyper server via explicit path to avoid re-export issues
    let server = server::Server::bind(&addr).serve(app.into_make_service());
    server.await?;

    Ok(())
}
