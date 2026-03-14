pub mod config;
pub mod shutdown;
pub mod state;

use axum::Router;
use std::net::SocketAddr;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::info;

use crate::{interfaces::http, services::ingestion_worker};

/// Boots the HTTP server and serves the `RustRAG` API.
///
/// # Errors
/// Returns any configuration, bind, listener, or serve error encountered during startup.
pub async fn run() -> anyhow::Result<()> {
    let config = config::Settings::from_env()?;
    crate::shared::telemetry::init(&config.log_filter);

    let state = state::AppState::new(config.clone()).await?;
    let shutdown = shutdown::ShutdownSignal::new();
    let worker_handle =
        ingestion_worker::spawn_ingestion_worker(state.clone(), shutdown.subscribe());
    let router = Router::new()
        .nest("/v1", http::router())
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = config.bind_addr.parse()?;
    info!(service = %config.service_name, environment = %config.environment, %addr, "starting rustrag backend");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    server.await?;
    shutdown.trigger();
    let _ = worker_handle.await;
    Ok(())
}
