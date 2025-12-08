
use std::sync::Arc;
use axum::{
    Router,
    routing::{post, get},
    extract::Extension,
};
use tracing_subscriber;
use std::net::SocketAddr;

mod config;
mod storage;
mod wbxml;
mod caldav;
mod ews;
mod eas;
mod sync;
mod models;
mod utils;

use config::Config;
use storage::Storage;
use models::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // load config
    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    let storage = Storage::new(&cfg.db_path).await?;
    storage.run_migrations().await?;

    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        storage: storage.clone(),
    });

    let app = Router::new()
        .route("/EWS/Exchange.asmx", post(ews::handle_ews))
        .route("/Microsoft-Server-ActiveSync", post(eas::handle_activesync))
        .route("/health", get(|| async { "OK" }))
        .layer(Extension(state));

    let addr: SocketAddr = cfg.http_bind.parse()?;
    println!("Listening on http://{}", addr);
    axum::Server::bind(&addr).serve(app.into_make_service()).await?;
    Ok(())
}
