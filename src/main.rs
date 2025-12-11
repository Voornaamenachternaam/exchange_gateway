use std::sync::Arc;
use axum::{
    Router,
    routing::{post, get},
    extract::Extension,
    Server,
};
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
mod ews_marshaller;

use config::Config;
use storage::Storage;
use models::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing/logging
    tracing_subscriber::fmt::init();

    // load config
    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    let storage_plain = Storage::new(&cfg.db_path).await?;
    storage_plain.run_migrations().await?;

    // store storage in Arc as AppState expects Arc<Storage>
    let storage = Arc::new(storage_plain);

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

    // Use axum::Server (re-export of hyper Server) which is compatible with axum versions
    Server::bind(&addr)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}
