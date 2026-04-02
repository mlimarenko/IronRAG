pub mod bootstrap;
pub mod config;
pub mod shutdown;
pub mod state;

use ::http::Response;
use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath},
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
    interfaces::http::{self, router_support},
    services::ingestion_worker,
};

/// Boots the HTTP server and serves the `RustRAG` API.
///
/// # Errors
/// Returns any configuration, bind, listener, or serve error encountered during startup.
pub async fn run() -> anyhow::Result<()> {
    let config = config::Settings::from_env()?;
    crate::shared::telemetry::init(&config.log_filter);

    let state = state::AppState::new(config.clone()).await?;
    if config.runs_http_api() {
        run_startup_bootstraps(
            &state,
            &config.bootstrap_settings(),
            &config.destructive_fresh_bootstrap_settings(),
        )
        .await?;
    }
    let graph_backend = state.graph_runtime.backend_name.as_str();
    let shutdown = shutdown::ShutdownSignal::new();
    let signal_listener = spawn_signal_listener(shutdown.clone());
    let worker_handle = config
        .runs_ingestion_workers()
        .then(|| ingestion_worker::spawn_ingestion_worker(state.clone(), shutdown.subscribe()));

    let run_result = if config.runs_http_api() {
        run_http_api(&config, &state, graph_backend, shutdown.clone()).await
    } else {
        info!(
            service = %config.service_name,
            service_role = %config.service_role,
            environment = %config.environment,
            graph_backend,
            worker_concurrency = config.ingestion_worker_concurrency.max(1),
            "starting rustrag worker service",
        );
        shutdown.wait().await;
        Ok(())
    };

    let _ = shutdown.trigger();
    signal_listener.abort();
    let _ = signal_listener.await;
    if let Some(worker_handle) = worker_handle {
        let _ = worker_handle.await;
    }
    run_result
}

fn build_router(config: &config::Settings, state: state::AppState) -> Router {
    let public_origin_settings = config.public_origin_settings();
    let max_request_body_bytes = state.mcp_memory.max_request_body_bytes();
    Router::new()
        .nest("/v1", http::router())
        .layer(DefaultBodyLimit::max(max_request_body_bytes))
        .layer(middleware::map_request(inject_request_id))
        .layer(middleware::map_response(propagate_request_id))
        .layer(
            CorsLayer::new()
                .allow_origin(parse_allowed_origins(&public_origin_settings))
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
        .with_state(state)
}

async fn run_http_api(
    config: &config::Settings,
    state: &state::AppState,
    graph_backend: &str,
    shutdown: shutdown::ShutdownSignal,
) -> anyhow::Result<()> {
    let router = build_router(config, state.clone());
    let addr: SocketAddr = config.bind_addr.parse()?;
    info!(
        service = %config.service_name,
        service_role = %config.service_role,
        environment = %config.environment,
        graph_backend,
        arangodb_url = %state.arango_runtime.url,
        arangodb_database = %state.arango_runtime.database,
        knowledge_backend = %state.graph_runtime.backend_name,
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
        mcp_memory_default_read_window_chars = state.mcp_memory.default_read_window_chars,
        mcp_memory_max_read_window_chars = state.mcp_memory.max_read_window_chars,
        mcp_memory_default_search_limit = state.mcp_memory.default_search_limit,
        mcp_memory_max_search_limit = state.mcp_memory.max_search_limit,
        mcp_memory_audit_enabled = state.mcp_memory.audit_enabled,
        minimum_slice_capacity = state.pipeline_hardening.minimum_slice_capacity,
        token_touch_min_interval_seconds = state.pipeline_hardening.token_touch_min_interval_seconds,
        heartbeat_write_min_interval_seconds = state
            .pipeline_hardening
            .heartbeat_write_min_interval_seconds,
        graph_progress_checkpoint_interval_seconds = state
            .pipeline_hardening
            .graph_progress_checkpoint_interval_seconds,
        graph_retry_limit = state.resolve_settle_blockers.projection_retry_limit,
        provider_request_size_soft_limit_bytes = state
            .resolve_settle_blockers
            .provider_request_size_soft_limit_bytes,
        provider_timeout_retry_limit = state
            .resolve_settle_blockers
            .provider_timeout_retry_limit,
        %addr,
        "starting rustrag backend"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown.wait().await;
    });
    server.await?;
    Ok(())
}

async fn run_startup_bootstraps(
    state: &state::AppState,
    bootstrap_settings: &config::BootstrapSettings,
    destructive_bootstrap: &config::DestructiveFreshBootstrapSettings,
) -> anyhow::Result<()> {
    let explicit_ui_bootstrap_admin = state.settings.has_explicit_ui_bootstrap_admin();
    if destructive_bootstrap.required
        && !destructive_bootstrap.allow_legacy_startup_side_effects
        && bootstrap_settings.legacy_ui_bootstrap_enabled
        && !explicit_ui_bootstrap_admin
    {
        anyhow::bail!(
            "destructive fresh bootstrap mode forbids startup bootstrap side effects; disable legacy_ui_bootstrap_enabled"
        );
    }

    if explicit_ui_bootstrap_admin {
        bootstrap::ensure_canonical_bootstrap_admin(state).await.map_err(|error| {
            anyhow::anyhow!("failed to initialize canonical bootstrap admin: {error}")
        })?;
    } else if bootstrap_settings.legacy_ui_bootstrap_enabled {
        if crate::infra::persistence::legacy_ui_bootstrap_tables_present(
            &state.persistence.postgres,
        )
        .await?
        {
            bootstrap::ensure_bootstrap_admin(state).await.map_err(|error| {
                anyhow::anyhow!("failed to initialize legacy bootstrap admin: {error}")
            })?;
        } else if crate::infra::persistence::canonical_ui_bootstrap_tables_present(
            &state.persistence.postgres,
        )
        .await?
        {
            bootstrap::ensure_canonical_bootstrap_admin(state).await.map_err(|error| {
                anyhow::anyhow!("failed to initialize canonical bootstrap admin: {error}")
            })?;
        } else {
            info!("bootstrap admin side effect not required at startup");
        }
    } else {
        info!("bootstrap admin side effect not required at startup");
    }

    info!("pricing catalog bootstrap side effect not required at startup");

    Ok(())
}

fn spawn_signal_listener(shutdown: shutdown::ShutdownSignal) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let signal_name = shutdown::wait_for_termination_signal().await;
        if shutdown.trigger() {
            warn!(signal = signal_name, "shutdown signal received");
        }
    })
}

fn parse_allowed_origins(origins: &config::PublicOriginSettings) -> AllowOrigin {
    let parsed_origins = origins
        .allowed_origins
        .iter()
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
