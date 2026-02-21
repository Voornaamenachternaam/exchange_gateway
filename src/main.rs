use axum::{
    Router,
    extract::Extension,
    routing::{get, post},
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

use config::Config;
use models::AppState;
use storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();

    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;

    // Storage now uses Cloudflare Worker URL + secret
    let storage_client = Storage::new(&cfg.worker_url, &cfg.worker_secret).await?;
    let storage = Arc::new(storage_client);

    // Run a lightweight self-check (no DB migrations locally)
    let wb = wbxml::Wbxml::new();
    let _ = wb.token_to_tag(4, 0x26);

    let state = Arc::new(models::AppState {
        cfg: cfg.clone(),
        storage,
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
