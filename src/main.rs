use std::sync::Arc;
use axum::{
    Router,
    routing::{post, get},
    extract::Extension,
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

    // Parse address and bind std listener
    let addr: SocketAddr = cfg.http_bind.parse()?;
    println!("Listening on http://{}", addr);

    // Create a std listener and set non-blocking so hyper can use it.
    let std_listener = std::net::TcpListener::bind(addr)?;
    std_listener.set_nonblocking(true)?;

    // Convert std listener to hyper's AddrIncoming and build server from it.
    // AddrIncoming::from_listener is stable for hyper 0.14.x
    let incoming = hyper::server::conn::AddrIncoming::from_listener(std_listener)?;
    hyper::Server::builder(incoming).serve(app.into_make_service()).await?;

    Ok(())
}
