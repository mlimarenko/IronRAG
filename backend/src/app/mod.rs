pub mod config;
pub mod shutdown;
pub mod state;

use ::http::Response;
use axum::{
    Router,
    extract::MatchedPath,
    http::{Request, header},
};
use std::{net::SocketAddr, time::Duration};
use tower_http::{classify::ServerErrorsFailureClass, cors::CorsLayer, trace::TraceLayer};
use tracing::{Span, error, info, warn};

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
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let matched_path = request.extensions().get::<MatchedPath>().map_or_else(
                        || "<unmatched>".to_string(),
                        |path| path.as_str().to_string(),
                    );
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                    )
                })
                .on_request(|request: &Request<_>, _span: &Span| {
                    let matched_path = request.extensions().get::<MatchedPath>().map_or_else(
                        || "<unmatched>".to_string(),
                        |path| path.as_str().to_string(),
                    );
                    let user_agent = request
                        .headers()
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    info!(
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        user_agent,
                        "http request started",
                    );
                })
                .on_response(|response: &Response<_>, latency: Duration, _span: &Span| {
                    let latency_ms = latency.as_millis();
                    let status = response.status();
                    if status.is_server_error() {
                        error!(%status, latency_ms, "http request completed with server error");
                    } else if status.is_client_error() {
                        warn!(%status, latency_ms, "http request completed with client error");
                    } else {
                        info!(%status, latency_ms, "http request completed");
                    }
                })
                .on_failure(
                    |failure_class: ServerErrorsFailureClass, latency: Duration, _span: &Span| {
                        error!(
                            %failure_class,
                            latency_ms = latency.as_millis(),
                            "http request failed before response",
                        );
                    },
                ),
        )
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
