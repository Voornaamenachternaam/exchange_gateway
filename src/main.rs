// src/main.rs
use axum::{
    routing::{post, any},
    Router,
};
use hyper::server::conn::Http;
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;
use std::sync::Arc;
use tokio::net::TcpListener;

mod config;
mod caldav;
mod storage;
mod models;
mod rrule_engine;
mod utils;
mod wbxml;
mod ews;
mod ews_marshaller;
mod eas;
mod sync;

use config::Config;
use models::AppState;
use storage::Storage;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing subscriber with environment filter
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Load gateway configuration
    let config = Config::load("/etc/exchange-gateway/config.toml")?;
    // Initialize storage with Cloudflare Worker endpoint and secret
    let storage = Storage::new(&config.worker_url, &config.worker_secret)?;
    let app_state = Arc::new(AppState { cfg: config.clone(), storage: Arc::new(storage) });

    // Build router for EWS and ActiveSync endpoints. Handlers use State<Arc<AppState>>.
    let app = Router::new()
        .route("/EWS/*path", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(eas::handle))
        .with_state(app_state.clone());

    // Bind to configured address (e.g., 0.0.0.0:8133)
    let addr: SocketAddr = config.bind.parse()?;
    tracing::info!("listening on http://{}", addr);

    // Create TcpListener and a MakeService which provides a per-connection Service.
    let listener = TcpListener::bind(addr).await?;
    let make_svc = app.into_make_service_with_connect_info::<SocketAddr>();

    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let mut make_svc = make_svc.clone();

        // Serve each connection in its own task.
        tokio::spawn(async move {
            // Build the per-connection service
            let svc = match make_svc.make_service(&remote_addr).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("make_service error for {}: {}", remote_addr, e);
                    return;
                }
            };

            // Serve the HTTP connection (HTTP/1 + HTTP/2 as negotiated)
            if let Err(err) = Http::new().serve_connection(stream, svc).await {
                tracing::error!("error serving connection from {}: {}", remote_addr, err);
            }
        });
    }
}
