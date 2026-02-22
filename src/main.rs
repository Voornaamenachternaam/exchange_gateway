// src/main.rs
use axum::{
    routing::{post, any},
    Router, Server,
};
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;
use std::sync::Arc;

mod config;
mod caldav;
mod storage;
mod models;
mod rrule_engine;
mod utils;
mod wbxml;
mod ews;
mod ews_marshaller;
mod eas;
mod sync;

use config::Config;
use models::AppState;
use storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber with environment filter
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load gateway configuration
    let config = Config::load("/etc/exchange-gateway/config.toml")?;
    // Initialize storage with Cloudflare Worker endpoint and secret
    let storage = Storage::new(&config.worker_url, &config.worker_secret)?;
    let app_state = Arc::new(AppState { cfg: config.clone(), storage: Arc::new(storage) });

    // Build router for EWS and ActiveSync endpoints. Handlers use State<Arc<AppState>>.
    let app = Router::new()
        .route("/EWS/*path", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state.clone());

    // Bind to configured address (e.g., 0.0.0.0:8133)
    let addr: SocketAddr = config.bind.parse()?;
    tracing::info!("listening on http://{}", addr);

    // Serve the application using axum::Server (stable public API)
    Server::bind(&addr).serve(app.into_make_service()).await?;

    Ok(())
}
