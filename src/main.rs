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
mod storage;
mod sync;
mod utils;
mod wbxml;
mod rrule_engine;

use config::Config;
use models::AppState;
use storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;

    // Initialize worker-backed storage (no local DB, no migrations)
    let storage_client = Storage::new(&cfg.worker_url, &cfg.worker_secret)?;
    let storage = Arc::new(storage_client);

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
    tracing::info!("Listening on http://{}", addr);

    axum::Server::bind(&addr).serve(app.into_make_service()).await?;
    Ok(())
}
