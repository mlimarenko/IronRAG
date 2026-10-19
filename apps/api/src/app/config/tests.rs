use super::*;
use super::{
    bootstrap::{
        is_removed_provider_api_key_env_name, parse_bootstrap_provider_api_keys_b64,
        parse_bootstrap_provider_api_keys_json,
    },
    loading::{ironrag_environment_source, settings_config_builder},
    validation::{
        validate_content_storage_settings, validate_credential_master_key,
        validate_database_settings, validate_graph_gc_settings, validate_ingestion_settings,
        validate_knowledge_plane_backend, validate_mcp_memory_settings, validate_oidc_settings,
        validate_provider_resilience_settings, validate_query_rerank_settings,
        validate_query_turn_admission_settings, validate_recognition_settings,
        validate_release_monitor_settings, validate_runtime_agent_settings, validate_service_name,
        validate_service_role, validate_ui_bootstrap_admin_settings,
    },
};
use config::Map;

fn sample_settings() -> Settings {
    Settings {
        bind_addr: "0.0.0.0:8080".into(),
        service_role: "api".into(),
        database_url: "postgres://postgres:postgres@127.0.0.1:5432/ironrag".into(),
        database_tls_leaf_certificate: None,
        database_max_connections: 20,
        api_replicas: 1,
        worker_replicas: 1,
        knowledge_plane_backend: "postgres".into(),
        redis_url: "redis://127.0.0.1:6379".into(),
        redis_tls_leaf_certificate: None,
        service_name: "ironrag-backend".into(),
        environment: "local".into(),
        log_filter: "info".into(),
        destructive_fresh_bootstrap_required: false,
        frontend_origin: "http://127.0.0.1:19000,http://localhost:19000".into(),
        openapi_public_origin: None,
        credential_master_key: None,
        credential_master_key_id: None,
        credential_previous_master_keys: None,
        ui_session_secret: "local-ui-session-secret".into(),
        ui_default_locale: "ru".into(),
        ui_bootstrap_admin_login: None,
        ui_bootstrap_admin_email: None,
        ui_bootstrap_admin_name: None,
        ui_bootstrap_admin_password: None,
        ui_bootstrap_admin_api_token: None,
        oidc_issuer_url: None,
        oidc_client_id: None,
        oidc_client_secret: None,
        oidc_redirect_url: None,
        oidc_required_group: None,
        oidc_bootstrap_admin_subject: None,
        ui_bootstrap_extract_text_provider_kind: None,
        ui_bootstrap_extract_text_model_name: None,
        ui_bootstrap_extract_graph_provider_kind: None,
        ui_bootstrap_extract_graph_model_name: None,
        ui_bootstrap_embed_chunk_provider_kind: None,
        ui_bootstrap_embed_chunk_model_name: None,
        ui_bootstrap_query_compile_provider_kind: None,
        ui_bootstrap_query_compile_model_name: None,
        ui_bootstrap_query_answer_provider_kind: None,
        ui_bootstrap_query_answer_model_name: None,
        ui_bootstrap_agent_provider_kind: None,
        ui_bootstrap_agent_model_name: None,
        ui_session_idle_ttl_hours: 72,
        ui_session_absolute_ttl_hours: 720,
        upload_max_size_mb: 50,
        recognition_default_raster_image_engine: "vision".into(),
        startup_authority_mode: "not_required".into(),
        dependency_postgres_mode: "external".into(),
        dependency_redis_mode: "external".into(),
        dependency_object_storage_mode: "disabled".into(),
        content_storage_provider: "filesystem".into(),
        content_storage_topology: "single_node".into(),
        content_storage_key_prefix: "".into(),
        content_storage_root: "/var/lib/ironrag/content-storage".into(),
        content_storage_s3_bucket: None,
        content_storage_s3_endpoint: None,
        content_storage_s3_region: Some("us-east-1".into()),
        content_storage_s3_access_key_id: None,
        content_storage_s3_secret_access_key: None,
        content_storage_s3_session_token: None,
        content_storage_s3_force_path_style: true,
        web_ingest_http_timeout_seconds: 20,
        web_ingest_max_redirects: 10,
        web_ingest_user_agent: "IronRAG-WebIngest/0.1".into(),
        web_ingest_crawl_concurrency: 4,
        ingestion_max_parallel_jobs_global: 64,
        ingestion_max_parallel_jobs_per_workspace: 16,
        ingestion_max_parallel_jobs_per_library: 4,
        ingestion_memory_soft_limit_mib: 0,
        ingestion_worker_lease_seconds: 300,
        ingestion_worker_heartbeat_interval_seconds: 15,
        maintenance_enabled: true,
        maintenance_tick_interval_seconds: 30,
        maintenance_class_interval_seconds: 3600,
        maintenance_stale_lease_seconds: 300,
        maintenance_full_pass_interval_seconds: 86_400,
        ingestion_embedding_parallelism: 2,
        ingestion_graph_extract_parallelism_per_doc: 2,
        llm_http_timeout_seconds: 120,
        runtime_agent_max_turns: 4,
        runtime_agent_max_parallel_actions: 4,
        runtime_trace_payload_budget_bytes: DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES,
        runtime_policy_reason_budget_chars: DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS,
        runtime_policy_reject_task_kinds: None,
        runtime_policy_reject_target_kinds: None,
        query_intent_cache_ttl_hours: 24,
        query_intent_cache_max_entries_per_library: 500,
        release_check_repository: "mlimarenko/IronRAG".into(),
        release_check_interval_hours: 12,
        graph_gc_hours: 24,
        query_rerank_enabled: true,
        query_rerank_candidate_limit: 24,
        query_semantic_rerank_mode: crate::domains::query::SemanticRerankMode::Off,
        query_semantic_rerank_timeout_ms: 1_000,
        query_semantic_rerank_candidate_limit: 16,
        query_semantic_rerank_candidate_text_chars: 1_200,
        query_semantic_rerank_total_text_chars: 18_000,
        query_balanced_context_enabled: true,
        runtime_graph_extract_recovery_enabled: true,
        runtime_graph_extract_recovery_max_attempts: 4,
        runtime_graph_extract_idle_timeout_seconds: 300,
        runtime_graph_extract_stage_timeout_seconds: 600,
        runtime_graph_extract_resume_downgrade_level_one_after_replays: 3,
        runtime_graph_extract_resume_downgrade_level_two_after_replays: 5,
        runtime_graph_summary_refresh_batch_size: 64,
        runtime_graph_targeted_reconciliation_enabled: true,
        runtime_graph_targeted_reconciliation_max_targets: 128,
        runtime_document_activity_freshness_seconds: 45,
        runtime_document_stalled_after_seconds: 180,
        runtime_graph_filter_empty_relations: true,
        runtime_graph_filter_degenerate_self_loops: true,
        runtime_graph_convergence_warning_backlog_threshold: 1,
        runtime_graph_projection_prewarm_enabled: false,
        runtime_graph_projection_prewarm_max_libraries: 0,
        mcp_memory_default_read_window_chars: 48_000,
        mcp_memory_max_read_window_chars: 192_000,
        mcp_memory_default_search_limit: 10,
        mcp_memory_max_search_limit: 25,
        mcp_memory_idempotency_retention_hours: 72,
        mcp_memory_audit_enabled: true,
        chunking_max_chars: 2800,
        chunking_overlap_chars: 280,
        provider_concurrency_max_outbound: 16,
        provider_concurrency_query_reserved: 4,
        provider_concurrency_acquire_timeout_ms: 30_000,
        provider_concurrency_registry_max_entries: 64,
        provider_concurrency_registry_idle_ttl_seconds: 900,
        provider_rate_limit_requests_per_second: 8,
        provider_rate_limit_burst: 16,
        provider_retry_max_attempts: 3,
        provider_retry_initial_backoff_ms: 200,
        provider_retry_max_backoff_ms: 5_000,
        query_turn_max_concurrent: 32,
        query_turn_admission_queue: 80,
        query_turn_admission_timeout_ms: 15_000,
        login_rate_limit_per_login_max_attempts: 5,
        login_rate_limit_per_login_window_seconds: 60,
        login_rate_limit_per_source_max_attempts: 20,
        login_rate_limit_per_source_window_seconds: 60,
        login_rate_limit_max_concurrent_verifications: 8,
        login_rate_limit_admission_queue: 32,
        login_rate_limit_admission_timeout_ms: 500,
        login_rate_limit_registry_max_entries: 10_000,
        login_rate_limit_registry_idle_ttl_seconds: 600,
        login_rate_limit_trusted_proxy_hops: 1,
    }
}

fn settings_from_env_entries(entries: &[(&str, &str)]) -> Settings {
    let mut env = Map::new();
    for (key, value) in entries {
        env.insert((*key).to_string(), (*value).to_string());
    }
    let cfg = settings_config_builder()
        .expect("defaults should build")
        .add_source(ironrag_environment_source(Some(env)))
        .build()
        .expect("config should build");
    let mut settings: Settings = cfg.try_deserialize().expect("settings should deserialize");
    settings.service_role = settings.service_role.trim().to_ascii_lowercase();
    settings.knowledge_plane_backend = settings.knowledge_plane_backend.trim().to_ascii_lowercase();
    validate_service_role(&settings).expect("role should validate");
    validate_knowledge_plane_backend(&settings).expect("knowledge backend should validate");
    validate_database_settings(&settings).expect("database settings should validate");
    validate_service_name(&settings).expect("service name should validate");
    validate_ingestion_settings(&settings).expect("ingestion settings should validate");
    validate_provider_resilience_settings(&settings)
        .expect("provider resilience settings should validate");
    validate_recognition_settings(&settings).expect("recognition settings should validate");
    validate_runtime_agent_settings(&settings).expect("runtime settings should validate");
    validate_query_rerank_settings(&settings).expect("query rerank settings should validate");
    validate_release_monitor_settings(&settings).expect("release monitor settings should validate");
    validate_graph_gc_settings(&settings).expect("graph GC settings should validate");
    validate_mcp_memory_settings(&settings).expect("mcp settings should validate");
    validate_oidc_settings(&settings).expect("OIDC settings should validate");
    settings
}

mod bootstrap;
mod loading;
mod runtime_validation;
