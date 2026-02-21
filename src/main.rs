use axum::{
    extract::Extension,
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;

mod caldav;
mod config;
mod eas;
mod ews;
mod ews_marshaller;
mod models;
mod sync;
mod storage;
mod utils;
mod wbxml;

use config::Config;
use models::AppState;
use storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Load configuration
    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    // Initialize storage (Cloudflare Worker-backed)
    let storage = Arc::new(Storage::new(&cfg));

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        storage,
    });

    let app = Router::new()
        .route("/EWS/Exchange.asmx", post(ews::handle_ews))
        .route("/Microsoft-Server-ActiveSync", post(eas::handle_activesync))
        .route("/health", get(|| async { "OK" }))
        .layer(Extension(state));

    let addr: SocketAddr = cfg.http_bind.parse()?;
    tracing::info!("Starting exchange_gateway on http://{}", addr);

    axum::Server::bind(&addr).serve(app.into_make_service()).await?;
    Ok(())
}
