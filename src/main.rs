// src/main.rs

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

async fn startup_self_check(cfg: &Config, storage: &Storage) {
    // Touch config fields to silence warnings
    let _ = &cfg.bind;
    let _ = &cfg.tls_cert;
    let _ = &cfg.tls_key;
    let _ = &cfg.log_level;
    let _ = &storage.db_path;
    let wb = wbxml::Wbxml::new();
    let _ = wb.encode("<test/>");
    let _ = wb.decode(b"<test/>");
    let _ = caldav::CaldavClient::new(cfg).find_user_calendars("dummy", "pass");
    let _ = Storage::get_sync_key;
    let _ = Storage::upsert_item_map;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    // Load configuration (e.g. from /etc/exchange-gateway/config.toml)
    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    let storage_plain = Storage::new(&cfg.db_path).await?;
    storage_plain.run_migrations().await?;
    let storage = Arc::new(storage_plain);
    let state = Arc::new(AppState {
        cfg: cfg.clone(),
        storage: storage.clone(),
    });

    // Perform dummy startup checks
    startup_self_check(&cfg, &storage).await;

    // Set up HTTP router
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
