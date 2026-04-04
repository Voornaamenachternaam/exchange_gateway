use axum::{
    routing::{get, post},
    Router,
};
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{info, warn};

mod caldav;
mod calendar;
mod config;
mod handlers;
mod models;
mod sync;
mod timezone;
mod wbxml;

use crate::config::Config;
use crate::handlers::{
    autodiscover_handler, ews_handler, health_handler, options_handler, status_handler,
    sync_handler,
};
use crate::models::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = Config::from_env_or_file()?;
    info!("Exchange Gateway starting on {}", cfg.bind);
    info!("CalDAV base: {}", cfg.caldav_base);
    info!("Worker URL: {}", cfg.worker_url);

    let state = Arc::new(AppState::new(cfg).await?);

    let app = Router::new()
        .route("/", get(status_handler))
        .route("/health", get(health_handler))
        .route("/Microsoft-Server-ActiveSync", post(sync_handler).get(status_handler))
        .route("/Microsoft-Server-ActiveSync/", post(sync_handler).get(status_handler))
        .route("/EWS/Exchange.asmx", post(ews_handler))
        .route("/EWS/Exchange.asmx/", post(ews_handler))
        .route("/ews/Exchange.asmx", post(ews_handler))
        .route("/ews/Exchange.asmx/", post(ews_handler))
        .route("/autodiscover/autodiscover.xml", post(autodiscover_handler).get(autodiscover_handler))
        .route("/autodiscover/autodiscover.json", get(autodiscover_handler))
        .route("/autodiscover/autodiscover.svc", post(autodiscover_handler))
        .route("/Autodiscover/Autodiscover.xml", post(autodiscover_handler).get(autodiscover_handler))
        .route("/Autodiscover/Autodiscover.json", get(autodiscover_handler))
        .route("/Autodiscover/Autodiscover.svc", post(autodiscover_handler))
        .fallback(options_handler)
        .with_state(state);

    let addr: SocketAddr = cfg.bind.parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    info!("Listening on http://{}", addr);

    axum::serve(listener, app).await?;
    Ok(())
}
