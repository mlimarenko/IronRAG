pub mod bootstrap;
pub mod config;
pub mod shutdown;
pub mod state;

use axum::Router;
use std::net::SocketAddr;
use tracing::{info, warn};

use crate::{
    domains::deployment::ServiceRole,
    infra::persistence::{run_postgres_migrations, validate_canonical_bootstrap_state},
    interfaces::http::{self, middleware::apply_canonical_middleware},
    services::content::storage::types::ContentStorageProbeStatus,
};

/// Boots the HTTP server and serves the `IronRAG` API.
///
/// # Errors
/// Returns any configuration, bind, listener, or serve error encountered during startup.
pub async fn run() -> anyhow::Result<()> {
    let config = config::Settings::from_env()?;
    let deployment_id = crate::observability::resolve_deployment_id(&config.database_url).await;
    crate::observability::init_tracing(deployment_id)?;
    let role = config.service_role_kind().map_err(anyhow::Error::msg)?;

    let state = state::AppState::new(config.clone()).await?;
    let graph_backend = state.graph_runtime.backend_name.as_str();
    let shutdown = shutdown::ShutdownSignal::new();
    let signal_listener = spawn_signal_listener(shutdown.clone());
    let worker_handle = role.runs_ingestion_workers().then(|| {
        crate::services::ingest::worker::spawn_ingestion_worker(state.clone(), shutdown.subscribe())
    });
    let maintenance_handle = crate::services::maintenance::scheduler::spawn_maintenance_scheduler(
        state.clone(),
        shutdown.subscribe(),
    );

    let run_result = match role {
        ServiceRole::Startup => {
            run_startup_authority(
                &state,
                &config.bootstrap_settings(),
                &config.destructive_fresh_bootstrap_settings(),
            )
            .await
        }
        ServiceRole::Api => run_http_api(&config, &state, graph_backend, shutdown.clone()).await,
        ServiceRole::Worker => {
            info!(
                service = %config.service_name,
                service_role = %config.service_role,
                environment = %config.environment,
                graph_backend,
                ingestion_max_parallel_jobs_global = config.ingestion_max_parallel_jobs_global,
                ingestion_max_parallel_jobs_per_workspace = config.ingestion_max_parallel_jobs_per_workspace,
                ingestion_max_parallel_jobs_per_library = config.ingestion_max_parallel_jobs_per_library,
                "starting ironrag worker service",
            );
            run_probe_http_api(&config, &state, graph_backend, shutdown.clone()).await
        }
    };

    let _ = shutdown.trigger();
    signal_listener.abort();
    let _ = signal_listener.await;
    if let Some(worker_handle) = worker_handle {
        let _ = worker_handle.await;
    }
    if let Some(maintenance_handle) = maintenance_handle {
        let _ = maintenance_handle.await;
    }
    crate::observability::shutdown_tracing().await;
    run_result
}

fn build_router(state: state::AppState) -> Router {
    build_http_router(state, false)
}

fn build_probe_router(state: state::AppState) -> Router {
    build_http_router(state, true)
}

fn build_http_router(state: state::AppState, probe_only: bool) -> Router {
    let api_router = if probe_only { http::probe_router() } else { http::router() };
    apply_canonical_middleware(Router::new().nest("/v1", api_router), state)
}

async fn run_http_api(
    config: &config::Settings,
    state: &state::AppState,
    graph_backend: &str,
    shutdown: shutdown::ShutdownSignal,
) -> anyhow::Result<()> {
    if state.settings.runtime_graph_projection_prewarm_enabled {
        spawn_runtime_graph_projection_prewarm(
            state.clone(),
            state.settings.runtime_graph_projection_prewarm_max_libraries,
        );
    }
    let router = build_router(state.clone());
    let addr: SocketAddr = config.bind_addr.parse()?;
    info!(
        service = %config.service_name,
        service_role = %config.service_role,
        environment = %config.environment,
        graph_backend,
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
        runtime_graph_projection_prewarm_enabled = state.settings.runtime_graph_projection_prewarm_enabled,
        runtime_graph_projection_prewarm_max_libraries = state.settings.runtime_graph_projection_prewarm_max_libraries,
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
        "starting ironrag backend"
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown.wait().await;
    });
    server.await?;
    Ok(())
}

async fn run_probe_http_api(
    config: &config::Settings,
    state: &state::AppState,
    graph_backend: &str,
    shutdown: shutdown::ShutdownSignal,
) -> anyhow::Result<()> {
    let router = build_probe_router(state.clone());
    let addr: SocketAddr = config.bind_addr.parse()?;
    info!(
        service = %config.service_name,
        service_role = %config.service_role,
        environment = %config.environment,
        graph_backend,
        %addr,
        "starting ironrag probe server",
    );
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown.wait().await;
    });
    server.await?;
    Ok(())
}

async fn run_startup_authority(
    state: &state::AppState,
    bootstrap_settings: &config::BootstrapSettings,
    destructive_bootstrap: &config::DestructiveFreshBootstrapSettings,
) -> anyhow::Result<()> {
    info!(
        service = %state.settings.service_name,
        service_role = %state.settings.service_role,
        environment = %state.settings.environment,
        startup_authority_mode = %state.settings.startup_authority_mode,
        "running startup authority",
    );

    run_postgres_migrations(&state.persistence.postgres).await?;
    validate_canonical_bootstrap_state(&state.persistence.postgres, &state.settings).await?;
    let storage_probe = state.content_storage.prepare_startup().await?;
    if !matches!(storage_probe.status, ContentStorageProbeStatus::Ok) {
        anyhow::bail!(
            "content storage startup validation failed: {}",
            storage_probe
                .message
                .unwrap_or_else(|| "provider did not report a healthy startup state".to_string())
        );
    }
    run_startup_bootstraps(state, bootstrap_settings, destructive_bootstrap).await?;
    info!("startup authority completed");
    Ok(())
}

async fn run_startup_bootstraps(
    state: &state::AppState,
    _bootstrap_settings: &config::BootstrapSettings,
    _destructive_bootstrap: &config::DestructiveFreshBootstrapSettings,
) -> anyhow::Result<()> {
    // Provider preset + env-keyed credential side effects must run on
    // every startup, regardless of whether a bootstrap admin login is
    // configured. Operators who created the admin via the UI still need
    // their `IRONRAG_<PROVIDER>_API_KEY` values to land as instance-scope
    // credentials and have the matching presets seeded; gating those on
    // `ui_bootstrap_admin` was the bug behind "I added a key and it
    // never appeared in admin → AI".
    state
        .canonical_services
        .ai_catalog
        .seed_all_provider_presets(state)
        .await
        .map_err(|error| anyhow::anyhow!("failed to seed provider presets: {error}"))?;
    state.canonical_services.ai_catalog.ensure_env_provider_credentials(state).await.map_err(
        |error| anyhow::anyhow!("failed to ensure env-keyed provider credentials: {error}"),
    )?;

    if state.ui_bootstrap_admin.is_some() {
        bootstrap::ensure_canonical_bootstrap_admin(state).await.map_err(|error| {
            anyhow::anyhow!("failed to initialize canonical bootstrap admin: {error}")
        })?;
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

/// Detached opt-in task that pre-loads the in-memory runtime graph
/// projection for active libraries on the backend role.
///
/// Without prewarm the first turn against a populated library incurs
/// a ~30+ s `plan.load_graph_index` cache miss while
/// `load_active_runtime_graph_projection` pulls hundreds of thousands
/// of edges and tens of thousands of nodes from Postgres. The miss
/// recurs after every backend restart and any time the 3-minute idle
/// TTL evicts an entry, even though steady-state queries hit a warm
/// cache cheaply.
///
/// Runs only when explicitly enabled. Large corpora can make all-library
/// prewarm allocate enough graph projection memory to OOM the API role; lazy
/// per-library loading remains the default path.
fn spawn_runtime_graph_projection_prewarm(state: state::AppState, max_libraries: usize) {
    tokio::spawn(async move {
        use futures::StreamExt;
        let prewarm_started = std::time::Instant::now();
        let mut libraries = match crate::infra::repositories::catalog_repository::list_libraries(
            &state.persistence.postgres,
            None,
        )
        .await
        {
            Ok(libraries) => libraries,
            Err(error) => {
                warn!(
                    error = format!("{error:#}"),
                    "runtime graph projection prewarm aborted: failed to list libraries"
                );
                return;
            }
        };
        let requested_library_count = libraries.len();
        if max_libraries > 0 && libraries.len() > max_libraries {
            libraries.truncate(max_libraries);
        }
        info!(
            stage = "graph_projection_prewarm_start",
            library_count = libraries.len(),
            requested_library_count,
            max_libraries,
            "runtime graph projection prewarm starting"
        );
        const PREWARM_CONCURRENCY: usize = 4;
        enum PrewarmOutcome {
            Warmed,
            Skipped,
            Failed,
        }
        let outcomes = futures::stream::iter(libraries.into_iter().map(|library| {
            let state = &state;
            async move {
                let library_id = library.id;
                let library_started = std::time::Instant::now();
                match crate::services::knowledge::runtime_read::load_active_runtime_graph_projection(
                    state, library_id,
                )
                .await
                {
                    Ok(projection) => {
                        let node_count = projection.nodes.len();
                        let edge_count = projection.edges.len();
                        if node_count == 0 && edge_count == 0 {
                            tracing::debug!(
                                stage = "graph_projection_prewarm_skip",
                                %library_id,
                                "runtime graph projection prewarm skipped empty library"
                            );
                            PrewarmOutcome::Skipped
                        } else {
                            info!(
                                stage = "graph_projection_prewarm_library",
                                %library_id,
                                node_count,
                                edge_count,
                                elapsed_ms = library_started.elapsed().as_millis() as u64,
                                "runtime graph projection prewarmed"
                            );
                            PrewarmOutcome::Warmed
                        }
                    }
                    Err(error) => {
                        warn!(
                            %library_id,
                            error = format!("{error:#}"),
                            "runtime graph projection prewarm failed for library"
                        );
                        PrewarmOutcome::Failed
                    }
                }
            }
        }))
        .buffer_unordered(PREWARM_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
        let mut warmed = 0_usize;
        let mut skipped = 0_usize;
        let mut failed = 0_usize;
        for outcome in outcomes {
            match outcome {
                PrewarmOutcome::Warmed => warmed += 1,
                PrewarmOutcome::Skipped => skipped += 1,
                PrewarmOutcome::Failed => failed += 1,
            }
        }
        info!(
            stage = "graph_projection_prewarm_done",
            warmed,
            skipped,
            failed,
            elapsed_ms = prewarm_started.elapsed().as_millis() as u64,
            "runtime graph projection prewarm complete"
        );
    });
}
