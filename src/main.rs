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
mod sync;
mod utils;
mod wbxml;

use config::Config;
use models::AppState;
use sync::Storage; // Storage is now our Cloudflare-backed storage

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    let storage = Arc::new(Storage::new(&cfg));

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

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
