pub mod config;
pub mod shutdown;
pub mod state;

use ::http::Response;
use axum::{
    Router,
    body::Body,
    extract::MatchedPath,
    http::{Method, Request, header},
    middleware,
};
use std::{net::SocketAddr, time::Duration};
use tower_http::{
    classify::ServerErrorsFailureClass,
    cors::{AllowOrigin, CorsLayer},
    trace::TraceLayer,
};
use tracing::{Span, error, info, warn};

use crate::{
    interfaces::http::{self, router_support, ui_auth},
    services::{ingestion_worker, pricing_catalog},
};

/// Boots the HTTP server and serves the `RustRAG` API.
///
/// # Errors
/// Returns any configuration, bind, listener, or serve error encountered during startup.
pub async fn run() -> anyhow::Result<()> {
    let config = config::Settings::from_env()?;
    crate::shared::telemetry::init(&config.log_filter);

    let state = state::AppState::new(config.clone()).await?;
    ui_auth::ensure_bootstrap_admin(&state)
        .await
        .map_err(|error| anyhow::anyhow!("failed to initialize bootstrap ui admin: {error}"))?;
    pricing_catalog::bootstrap_from_env_if_enabled(&state)
        .await
        .map_err(|error| anyhow::anyhow!("failed to bootstrap pricing catalog: {error}"))?;
    let graph_backend = state.graph_store.backend_name();
    let shutdown = shutdown::ShutdownSignal::new();
    let worker_handle =
        ingestion_worker::spawn_ingestion_worker(state.clone(), shutdown.subscribe());
    let router = Router::new()
        .nest("/v1", http::router())
        .layer(middleware::map_request(inject_request_id))
        .layer(middleware::map_response(propagate_request_id))
        .layer(
            CorsLayer::new()
                .allow_origin(parse_allowed_origins(&config.frontend_origin))
                .allow_credentials(true)
                .allow_methods([
                    Method::GET,
                    Method::POST,
                    Method::PUT,
                    Method::DELETE,
                    Method::OPTIONS,
                ])
                .allow_headers([
                    header::ACCEPT,
                    header::AUTHORIZATION,
                    header::CONTENT_TYPE,
                    header::HeaderName::from_static(router_support::REQUEST_ID_HEADER),
                ]),
        )
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &Request<_>| {
                    let matched_path = request.extensions().get::<MatchedPath>().map_or_else(
                        || "<unmatched>".to_string(),
                        |path| path.as_str().to_string(),
                    );
                    let request_id = request
                        .extensions()
                        .get::<router_support::RequestId>()
                        .map(|request_id| request_id.0.clone())
                        .unwrap_or_else(|| "-".to_string());
                    tracing::info_span!(
                        "http_request",
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        request_id,
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
                    let request_id = request
                        .extensions()
                        .get::<router_support::RequestId>()
                        .map(|request_id| request_id.0.as_str())
                        .unwrap_or("-");
                    info!(
                        method = %request.method(),
                        matched_path,
                        uri = %request.uri(),
                        user_agent,
                        request_id,
                        "http request started",
                    );
                })
                .on_response(|response: &Response<_>, latency: Duration, _span: &Span| {
                    let latency_ms = latency.as_millis();
                    let status = response.status();
                    let request_id = response
                        .headers()
                        .get(router_support::REQUEST_ID_HEADER)
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("-");
                    if status.is_server_error() {
                        error!(%status, latency_ms, request_id, "http request completed with server error");
                    } else if status.is_client_error() {
                        warn!(%status, latency_ms, request_id, "http request completed with client error");
                    } else {
                        info!(%status, latency_ms, request_id, "http request completed");
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
        .with_state(state.clone());

    let addr: SocketAddr = config.bind_addr.parse()?;
    info!(
        service = %config.service_name,
        environment = %config.environment,
        graph_backend,
        neo4j_uri = %state.graph_runtime.neo4j_uri,
        neo4j_database = %state.graph_runtime.neo4j_database,
        query_intent_cache_ttl_hours = state.retrieval_intelligence.query_intent_cache_ttl_hours,
        rerank_enabled = state.retrieval_intelligence.rerank_enabled,
        extraction_recovery_enabled = state.retrieval_intelligence.extraction_recovery_enabled,
        targeted_reconciliation_enabled = state.retrieval_intelligence.targeted_reconciliation_enabled,
        document_activity_freshness_seconds = state
            .bulk_ingest_hardening
            .document_activity_freshness_seconds,
        document_stalled_after_seconds = state.bulk_ingest_hardening.document_stalled_after_seconds,
        graph_filter_empty_relations = state.bulk_ingest_hardening.graph_filter_empty_relations,
        graph_filter_degenerate_self_loops = state
            .bulk_ingest_hardening
            .graph_filter_degenerate_self_loops,
        %addr,
        "starting rustrag backend"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
    });
    server.await?;
    shutdown.trigger();
    let _ = worker_handle.await;
    Ok(())
}

fn parse_allowed_origins(origins: &str) -> AllowOrigin {
    let parsed_origins = origins
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| value.parse().ok())
        .collect::<Vec<header::HeaderValue>>();

    if parsed_origins.is_empty() {
        return AllowOrigin::list([
            header::HeaderValue::from_static("http://127.0.0.1:19000"),
            header::HeaderValue::from_static("http://localhost:19000"),
        ]);
    }

    AllowOrigin::list(parsed_origins)
}

async fn inject_request_id(mut request: Request<Body>) -> Request<Body> {
    let request_id = router_support::ensure_or_generate_request_id(request.headers());
    router_support::attach_request_id_header(request.headers_mut(), &request_id);
    request.extensions_mut().insert(router_support::RequestId(request_id));
    request
}

async fn propagate_request_id(response: Response<Body>) -> Response<Body> {
    response
}
