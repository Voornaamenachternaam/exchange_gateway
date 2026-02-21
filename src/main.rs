use axum::{
    routing::{post, any},
    Router,
};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

mod ews;
mod activesync;
mod config;
mod sync;
mod worker_client;

use config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let config = Config::load("config.toml")?;

    let app = Router::new()
        .route("/EWS/*path", post(ews::handle))
        .route("/Microsoft-Server-ActiveSync", any(activesync::handle))
        .with_state(config);

    let addr: SocketAddr = "0.0.0.0:8080".parse()?;
    let listener = TcpListener::bind(addr).await?;

    axum::serve(listener, app).await?;

    Ok(())
}
