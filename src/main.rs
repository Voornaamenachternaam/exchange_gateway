use std::sync::Arc;
use warp::Filter;

mod caldav;
mod ews;
mod eas;
mod config;

use crate::config::Config;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cfg = Config::load("/etc/exchange-gateway/config.toml")?;

    // Shared application state
    let app = Arc::new(caldav::AppState::new(cfg.clone()).await?);

    // Health
    let health = warp::path("health").and_then(|| async move {
        Ok::<_, warp::Rejection>(warp::reply::with_status("OK", warp::http::StatusCode::OK))
    });

    // EWS route (SOAP over POST)
    let ews_state = app.clone();
    let ews_route = warp::path("EWS")
        .and(warp::path("Exchange.asmx"))
        .and(warp::post())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::body::bytes())
        .and_then(move |auth: Option<String>, body: bytes::Bytes| {
            let state = ews_state.clone();
            async move {
                ews::handle_ews(state, auth, body).await
            }
        });

    // ActiveSync route
    let eas_state = app.clone();
    let eas_route = warp::path("Microsoft-Server-ActiveSync")
        .and(warp::post())
        .and(warp::header::optional::<String>("authorization"))
        .and(warp::body::bytes())
        .and_then(move |auth: Option<String>, body: bytes::Bytes| {
            let state = eas_state.clone();
            async move {
                eas::handle_activesync(state, auth, body).await
            }
        });

    // HTTP management (non-TLS)
    let http_addr = cfg.http_bind.parse()?;
    let routes = health.or(ews_route).or(eas_route);

    // Spawn non-TLS HTTP server for health and optional redirect
    let (http_done_tx, http_done_rx) = tokio::sync::oneshot::channel::<()>();
    let http_server = warp::serve(routes.clone()).bind_with_graceful_shutdown(http_addr, async {
        let _ = http_done_rx.await;
    });

    // TLS server for production (use cert/key from config)
    let tls_addr = cfg.bind.parse()?;
    let tls_routes = routes; // same handlers on TLS

    // Use warp's TLS support via native-tls/hyper -- warp doesn't expose direct TLS,
    // so we will run the TLS server via hyper + rustls in production code (omitted here).
    // For now run the non-TLS HTTP server (container fronted by cloudflared tunnels & TLS).
    tracing::info!("Starting HTTP server on {}", cfg.http_bind);
    http_server.await;

    // Signal shutdown
    let _ = http_done_tx.send(());

    Ok(())
}
