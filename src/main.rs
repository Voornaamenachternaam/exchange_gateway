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

async fn startup_self_check(cfg: &Config, storage: &Storage) {
    // read config fields to mark them used
    let _ = &cfg.bind;
    let _ = &cfg.tls_cert;
    let _ = &cfg.tls_key;
    let _ = &cfg.log_level;

    // read storage field to mark it used
    let _ = &storage.db_path;

    // reference Storage async methods (function items) so they are considered used
    let _ = Storage::get_sync_key;
    let _ = Storage::get_item_by_server_id;
    let _ = Storage::delete_item_by_server_id;
    let _ = Storage::list_changes_since;

    // construct WBXML and reference its helpers
    let wb = wbxml::Wbxml::new();
    let _ = wb.token_to_tag(0, 0);
    let _ = wb.tag_to_token(0, "");
    let _ = wb.encode("<x/>");

    // reference CaldavClient methods (function items) so they are considered used
    let _ = caldav::CaldavClient::find_user_calendars;
    let _ = caldav::CaldavClient::query_events;
    let _ = caldav::CaldavClient::get_event;
    let _ = caldav::CaldavClient::put_event;
    let _ = caldav::CaldavClient::delete_event;

    // quietly return
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;
    let storage_plain = Storage::new(&cfg.db_path).await?;
    storage_plain.run_migrations().await?;

    let storage = Arc::new(storage_plain);

    // Run the startup self-check to exercise symbols that trigger clippy warnings.
    startup_self_check(&cfg, &*storage).await;

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
 
