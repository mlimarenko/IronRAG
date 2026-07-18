use super::*;
use config::Map;

fn sample_settings() -> Settings {
    Settings {
        bind_addr: "0.0.0.0:8080".into(),
        service_role: "api".into(),
        database_url: "postgres://postgres:postgres@127.0.0.1:5432/ironrag".into(),
        database_max_connections: 20,
        api_replicas: 1,
        worker_replicas: 1,
        knowledge_plane_backend: "postgres".into(),
        redis_url: "redis://127.0.0.1:6379".into(),
        service_name: "ironrag-backend".into(),
        environment: "local".into(),
        log_filter: "info".into(),
        destructive_fresh_bootstrap_required: false,
        frontend_origin: "http://127.0.0.1:19000,http://localhost:19000".into(),
        openapi_public_origin: None,
        credential_master_key: None,
        credential_master_key_id: None,
        credential_previous_master_keys: None,
        credential_encryption_write_enabled: false,
        ui_session_secret: "local-ui-session-secret".into(),
        ui_default_locale: "ru".into(),
        ui_bootstrap_admin_login: None,
        ui_bootstrap_admin_email: None,
        ui_bootstrap_admin_name: None,
        ui_bootstrap_admin_password: None,
        ui_bootstrap_admin_api_token: None,
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
        ui_session_ttl_hours: 720,
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
        query_semantic_rerank_timeout_ms: 1_500,
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
    }
}

fn settings_from_env_entries(entries: &[(&str, &str)]) -> Settings {
    let mut env = Map::new();
    for (key, value) in entries {
        env.insert((*key).to_string(), (*value).to_string());
    }
    let cfg = settings_config_builder()
        .expect("defaults should build")
        .add_source(
            config::Environment::with_prefix("IRONRAG")
                .prefix_separator("_")
                .separator("__")
                .source(Some(env)),
        )
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
    validate_provider_concurrency_settings(&settings)
        .expect("provider concurrency settings should validate");
    validate_recognition_settings(&settings).expect("recognition settings should validate");
    validate_runtime_agent_settings(&settings).expect("runtime settings should validate");
    validate_query_rerank_settings(&settings).expect("query rerank settings should validate");
    validate_release_monitor_settings(&settings).expect("release monitor settings should validate");
    validate_graph_gc_settings(&settings).expect("graph GC settings should validate");
    validate_mcp_memory_settings(&settings).expect("mcp settings should validate");
    settings
}

#[test]
fn from_env_has_sane_local_defaults() {
    // Hermetic on purpose: Settings::from_env() reads the real process
    // environment, so any ambient IRONRAG_* variable (CI service wiring,
    // developer shells) would flip these default assertions.
    let settings = settings_from_env_entries(&[]);

    assert_eq!(settings.bind_addr, "0.0.0.0:8080");
    assert_eq!(settings.service_role, "api");
    assert_eq!(settings.service_name, "ironrag-backend");
    assert_eq!(settings.environment, "local");
    assert_eq!(settings.database_max_connections, 20);
    assert_eq!(settings.api_replicas, 1);
    assert_eq!(settings.worker_replicas, 1);
    assert_eq!(settings.knowledge_plane_backend, "postgres");
    assert_eq!(settings.ingestion_graph_extract_parallelism_per_doc, 16);
    assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
    assert_eq!(settings.log_filter, "info");
    assert!(settings.credential_master_key.is_none());
    assert!(settings.credential_master_key_id.is_none());
    assert!(settings.credential_previous_master_keys.is_none());
    assert!(!settings.credential_encryption_write_enabled);
    assert_eq!(settings.ingestion_max_parallel_jobs_global, 64);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_workspace, 16);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_library, 4);
    assert_eq!(settings.ingestion_memory_soft_limit_mib, 0);
    assert_eq!(settings.provider_concurrency_max_outbound, 16);
    assert_eq!(settings.provider_concurrency_query_reserved, 4);
    assert_eq!(settings.provider_concurrency_acquire_timeout_ms, 30_000);
    assert_eq!(settings.provider_concurrency_registry_max_entries, 64);
    assert_eq!(settings.provider_concurrency_registry_idle_ttl_seconds, 900);
    assert_eq!(settings.runtime_agent_max_turns, 4);
    assert_eq!(settings.runtime_graph_extract_idle_timeout_seconds, 300);
    assert_eq!(settings.release_check_repository, "mlimarenko/IronRAG");
    assert_eq!(settings.release_check_interval_hours, 12);
    assert_eq!(settings.graph_gc_hours, 24);
    assert_eq!(settings.runtime_agent_max_parallel_actions, 4);
    assert_eq!(settings.recognition_default_raster_image_engine, "vision");
    assert_eq!(
        settings.default_recognition_policy().raster_image_engine,
        RecognitionEngine::Vision
    );
    assert_eq!(
        settings.runtime_trace_payload_budget_bytes,
        DEFAULT_RUNTIME_DIAGNOSTIC_PAYLOAD_BUDGET_BYTES
    );
    assert_eq!(
        settings.runtime_policy_reason_budget_chars,
        DEFAULT_RUNTIME_POLICY_REASON_BUDGET_CHARS
    );
    assert_eq!(settings.query_intent_cache_ttl_hours, 24);
    assert!(settings.query_rerank_enabled);
    assert_eq!(settings.query_semantic_rerank_mode, crate::domains::query::SemanticRerankMode::Off);
    assert_eq!(settings.query_semantic_rerank_timeout_ms, 1_500);
    assert_eq!(settings.query_semantic_rerank_candidate_limit, 16);
    assert_eq!(settings.query_semantic_rerank_candidate_text_chars, 1_200);
    assert_eq!(settings.query_semantic_rerank_total_text_chars, 18_000);
    assert!(settings.runtime_graph_extract_recovery_enabled);
    assert_eq!(settings.content_storage_root, "/var/lib/ironrag/content-storage");
    assert_eq!(settings.runtime_document_activity_freshness_seconds, 90);
    assert_eq!(settings.runtime_document_stalled_after_seconds, 240);
    assert!(settings.runtime_graph_filter_empty_relations);
    assert!(settings.runtime_graph_filter_degenerate_self_loops);
    assert_eq!(settings.runtime_graph_convergence_warning_backlog_threshold, 1);
    assert!(!settings.runtime_graph_projection_prewarm_enabled);
    assert_eq!(settings.runtime_graph_projection_prewarm_max_libraries, 0);
    assert_eq!(settings.mcp_memory_default_read_window_chars, 48_000);
    assert_eq!(settings.mcp_memory_max_read_window_chars, 192_000);
    assert_eq!(settings.mcp_memory_default_search_limit, 10);
    assert_eq!(settings.mcp_memory_max_search_limit, 25);
    assert_eq!(settings.mcp_memory_idempotency_retention_hours, 72);
    assert!(settings.mcp_memory_audit_enabled);
}

#[test]
fn provider_concurrency_validation_rejects_deadlock_and_unbounded_registry_shapes() {
    let mut settings = sample_settings();
    settings.provider_concurrency_max_outbound = 4;
    settings.provider_concurrency_query_reserved = 4;
    assert!(validate_provider_concurrency_settings(&settings).is_err());

    settings.provider_concurrency_max_outbound = 0;
    settings.provider_concurrency_query_reserved = 1;
    assert!(validate_provider_concurrency_settings(&settings).is_err());

    settings.provider_concurrency_max_outbound = 4;
    settings.provider_concurrency_query_reserved = 1;
    settings.provider_concurrency_registry_max_entries = 0;
    assert!(validate_provider_concurrency_settings(&settings).is_err());
}

#[test]
fn provider_concurrency_environment_overrides_are_typed() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND", "12"),
        ("IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED", "3"),
        ("IRONRAG_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS", "5000"),
        ("IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES", "32"),
        ("IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_IDLE_TTL_SECONDS", "120"),
    ]);

    assert_eq!(settings.provider_concurrency_max_outbound, 12);
    assert_eq!(settings.provider_concurrency_query_reserved, 3);
    assert_eq!(settings.provider_concurrency_acquire_timeout_ms, 5_000);
    assert_eq!(settings.provider_concurrency_registry_max_entries, 32);
    assert_eq!(settings.provider_concurrency_registry_idle_ttl_seconds, 120);
}

#[test]
fn credential_master_key_validation_is_strict_and_redacted() {
    let mut settings = sample_settings();
    settings.credential_master_key = Some("invalid-value-that-must-not-appear".to_string());

    let error = validate_credential_master_key(&settings).expect_err("invalid key must fail");

    assert!(!error.contains("invalid-value-that-must-not-appear"));
    assert!(error.contains("IRONRAG_CREDENTIAL_MASTER_KEY"));
}

#[test]
fn credential_keyring_env_is_opt_in_bounded_and_strictly_canonical() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let active_key = STANDARD.encode([41_u8; 32]);
    let older_key = STANDARD.encode([42_u8; 32]);
    let previous_map = format!("old-2026={older_key}");
    let settings = settings_from_env_entries(&[
        ("IRONRAG_CREDENTIAL_MASTER_KEY", &active_key),
        ("IRONRAG_CREDENTIAL_MASTER_KEY_ID", "new-2026"),
        ("IRONRAG_CREDENTIAL_PREVIOUS_MASTER_KEYS", &previous_map),
    ]);

    assert_eq!(settings.credential_master_key_id.as_deref(), Some("new-2026"));
    assert_eq!(settings.credential_previous_master_keys.as_deref(), Some(previous_map.as_str()));
    validate_credential_master_key(&settings).expect("canonical keyring should validate");

    let mut duplicate = sample_settings();
    duplicate.credential_master_key = Some(active_key.clone());
    duplicate.credential_master_key_id = Some("new-2026".into());
    duplicate.credential_previous_master_keys =
        Some(format!("old-2026={older_key},old-2026={}", STANDARD.encode([43_u8; 32])));
    assert!(validate_credential_master_key(&duplicate).is_err());

    let mut missing_active = sample_settings();
    missing_active.credential_master_key_id = Some("new-2026".into());
    missing_active.credential_previous_master_keys = Some(previous_map);
    assert!(validate_credential_master_key(&missing_active).is_err());
}

#[test]
fn credential_encryption_write_gate_defaults_closed_and_is_operator_configurable() {
    let default_settings = settings_from_env_entries(&[]);
    assert!(!default_settings.credential_encryption_write_enabled);

    let enabled_settings = settings_from_env_entries(&[
        ("IRONRAG_CREDENTIAL_MASTER_KEY", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        ("IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED", "true"),
    ]);
    assert!(enabled_settings.credential_encryption_write_enabled);
}

#[test]
fn credential_encryption_write_gate_requires_an_active_key() {
    let mut settings = sample_settings();
    settings.credential_encryption_write_enabled = true;

    let error = validate_credential_master_key(&settings)
        .expect_err("enabling credential writes without an active key must fail");

    assert!(error.contains("IRONRAG_CREDENTIAL_MASTER_KEY is required"));
    assert!(error.contains("IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=true"));
}

#[test]
fn credential_keyring_validation_errors_never_include_key_material() {
    let mut settings = sample_settings();
    settings.credential_master_key = Some("active-secret-regression".into());
    settings.credential_master_key_id = Some("Current-Key".into());
    settings.credential_previous_master_keys = Some("old=previous-secret-regression".into());

    let error = validate_credential_master_key(&settings).expect_err("invalid keyring must fail");

    assert!(!error.contains("active-secret-regression"));
    assert!(!error.contains("previous-secret-regression"));
    assert!(!error.contains("Current-Key"));
}

#[test]
fn bootstrap_admin_debug_and_retained_settings_redact_secrets() {
    let admin = UiBootstrapAdmin {
        login: "owner".into(),
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        password: "bootstrap-password-regression".into(),
        api_token: Some("bootstrap-token-regression".into()),
    };
    let debug = format!("{admin:?}");
    assert!(!debug.contains("bootstrap-password-regression"));
    assert!(!debug.contains("bootstrap-token-regression"));

    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_password = Some("retained-password-regression".into());
    settings.ui_bootstrap_admin_api_token = Some("retained-token-regression".into());
    settings.discard_ui_bootstrap_admin_secrets();
    assert!(settings.ui_bootstrap_admin_password.is_none());
    assert!(settings.ui_bootstrap_admin_api_token.is_none());
}

#[test]
fn database_only_settings_can_discard_the_raw_credential_key() {
    let mut settings = sample_settings();
    settings.credential_master_key = Some("synthetic-config-key".to_string());
    settings.credential_master_key_id = Some("current".to_string());
    settings.credential_previous_master_keys = Some("old=synthetic-previous-key".to_string());

    settings.discard_credential_master_key();

    assert!(settings.credential_master_key.is_none());
    assert!(settings.credential_master_key_id.is_none());
    assert!(settings.credential_previous_master_keys.is_none());
    assert!(settings.clone().credential_master_key.is_none());
    assert!(settings.clone().credential_previous_master_keys.is_none());
}

#[test]
fn semantic_rerank_mode_is_typed_and_operator_configurable() {
    let settings = settings_from_env_entries(&[("IRONRAG_QUERY_SEMANTIC_RERANK_MODE", "shadow")]);

    assert_eq!(
        settings.query_semantic_rerank_mode,
        crate::domains::query::SemanticRerankMode::Shadow
    );
}

#[test]
fn semantic_rerank_provider_modes_require_the_master_rerank_gate() {
    let mut settings = sample_settings();
    settings.query_rerank_enabled = false;
    settings.query_semantic_rerank_mode = crate::domains::query::SemanticRerankMode::Active;

    let error = validate_query_rerank_settings(&settings)
        .expect_err("provider mode must not be silently disabled by the master gate");

    assert!(error.contains("query_rerank_enabled=true"));
}

#[test]
fn recognition_default_raster_image_engine_overrides_default() {
    let settings = settings_from_env_entries(&[(
        "IRONRAG_RECOGNITION_DEFAULT_RASTER_IMAGE_ENGINE",
        "docling",
    )]);

    assert_eq!(
        settings.default_recognition_policy().raster_image_engine,
        RecognitionEngine::Docling
    );
}

#[test]
fn recognition_default_raster_image_engine_rejects_unsupported_native() {
    let mut settings = sample_settings();
    settings.recognition_default_raster_image_engine = "native".to_string();

    let error = validate_recognition_settings(&settings).expect_err("native must be rejected");
    assert!(error.contains("rasterImageEngine must be either docling or vision"));
}

#[test]
fn from_env_provides_default_database_url() {
    // Hermetic for the same reason as from_env_has_sane_local_defaults.
    let settings = settings_from_env_entries(&[]);

    assert_eq!(settings.database_url, "postgres://postgres:postgres@127.0.0.1:5432/ironrag");
}

#[test]
fn canonical_prefixed_flat_variables_override_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_DATABASE_URL", "postgres://postgres:postgres@postgres:5432/ironrag"),
        ("IRONRAG_SERVICE_ROLE", "API"),
        ("IRONRAG_LOG_FILTER", "debug"),
    ]);

    assert_eq!(settings.database_url, "postgres://postgres:postgres@postgres:5432/ironrag");
    assert_eq!(settings.service_role, "api");
    assert_eq!(settings.log_filter, "debug");
}

#[test]
fn database_connection_budget_must_cover_runtime_replicas() {
    let mut settings = sample_settings();
    settings.database_max_connections = 7;

    assert_eq!(
        validate_database_settings(&settings),
        Err("database_max_connections must be at least 8 for api_replicas=1 and worker_replicas=1"
            .into()),
    );
}

#[test]
fn database_replica_counts_override_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_API_REPLICAS", "2"),
        ("IRONRAG_WORKER_REPLICAS", "3"),
        ("IRONRAG_DATABASE_MAX_CONNECTIONS", "20"),
    ]);

    assert_eq!(settings.api_replicas, 2);
    assert_eq!(settings.worker_replicas, 3);
    assert_eq!(settings.database_max_connections, 20);
}

#[test]
fn knowledge_plane_backend_env_overrides_default() {
    let settings = settings_from_env_entries(&[("IRONRAG_KNOWLEDGE_PLANE_BACKEND", " POSTGRES ")]);

    assert_eq!(settings.knowledge_plane_backend, "postgres");
}

#[test]
fn rejects_invalid_knowledge_plane_backend() {
    let mut settings = sample_settings();
    settings.knowledge_plane_backend = "mysql".into();

    let error = validate_knowledge_plane_backend(&settings).expect_err("backend should fail");
    assert!(error.contains("knowledge_plane_backend"));
}

#[test]
fn canonical_ingestion_limit_variables_override_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_INGESTION_MAX_PARALLEL_JOBS_GLOBAL", "600"),
        ("IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_WORKSPACE", "144"),
        ("IRONRAG_INGESTION_MAX_PARALLEL_JOBS_PER_LIBRARY", "24"),
    ]);

    assert_eq!(settings.ingestion_max_parallel_jobs_global, 600);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_workspace, 144);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_library, 24);
}

#[test]
fn canonical_graph_gc_interval_variable_overrides_default() {
    let settings = settings_from_env_entries(&[("IRONRAG_GRAPH_GC_HOURS", "6")]);

    assert_eq!(settings.graph_gc_hours, 6);
}

#[test]
fn runtime_graph_projection_prewarm_variables_override_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_ENABLED", "true"),
        ("IRONRAG_RUNTIME_GRAPH_PROJECTION_PREWARM_MAX_LIBRARIES", "2"),
    ]);

    assert!(settings.runtime_graph_projection_prewarm_enabled);
    assert_eq!(settings.runtime_graph_projection_prewarm_max_libraries, 2);
}

#[test]
fn ingestion_limits_must_nest_from_library_to_global() {
    let mut settings = sample_settings();
    settings.ingestion_max_parallel_jobs_global = 64;
    settings.ingestion_max_parallel_jobs_per_workspace = 96;

    assert_eq!(
        validate_ingestion_settings(&settings),
        Err(
            "ingestion_max_parallel_jobs_per_workspace must be less than or equal to ingestion_max_parallel_jobs_global"
                .into(),
        ),
    );

    settings.ingestion_max_parallel_jobs_per_workspace = 32;
    settings.ingestion_max_parallel_jobs_per_library = 48;

    assert_eq!(
        validate_ingestion_settings(&settings),
        Err(
            "ingestion_max_parallel_jobs_per_library must be less than or equal to ingestion_max_parallel_jobs_per_workspace"
                .into(),
        ),
    );
}

#[test]
fn resolved_ui_bootstrap_admin_is_absent_without_explicit_credentials() {
    let settings = sample_settings();

    assert_eq!(settings.resolved_ui_bootstrap_admin(), None);
    assert!(!settings.has_explicit_ui_bootstrap_admin());
}

#[test]
fn resolved_ui_bootstrap_admin_uses_configured_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" root ".into());
    settings.ui_bootstrap_admin_email = Some(" admin@example.com ".into());
    settings.ui_bootstrap_admin_name = Some(" Platform Owner ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());
    settings.ui_bootstrap_admin_api_token = Some(" bootstrap-token ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_admin(),
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "admin@example.com".into(),
            display_name: "Platform Owner".into(),
            password: "secret".into(),
            api_token: Some("bootstrap-token".into()),
        })
    );
    assert!(settings.has_explicit_ui_bootstrap_admin());
}

#[test]
fn resolved_ui_bootstrap_admin_derives_email_when_missing() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" owner ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_admin(),
        Some(UiBootstrapAdmin {
            login: "owner".into(),
            email: "owner@ironrag.local".into(),
            display_name: "Admin".into(),
            password: "secret".into(),
            api_token: None,
        })
    );
}

#[test]
fn resolved_ui_bootstrap_ai_is_absent_without_provider_credentials() {
    let settings = sample_settings();

    assert_eq!(settings.resolved_ui_bootstrap_ai_setup(), None);
}

#[test]
fn removed_provider_secret_env_convention_is_detected_structurally() {
    assert!(is_removed_provider_api_key_env_name("IRONRAG_PROVIDER_7_API_KEY"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_API_KEY"));
    assert!(!is_removed_provider_api_key_env_name("IRONRAG_provider_API_KEY"));
}

#[test]
fn bootstrap_provider_secrets_preserve_arbitrary_provider_kinds_without_collisions() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let encoded_map = STANDARD.encode(
        r#"{
            "zeta-gateway": "  zeta-secret  ",
            "alpha.7": "alpha-secret",
            "zeta_gateway": "underscore-secret",
            "Zeta/Gateway": "value$#\"\\юникод",
            "whitespace-only": "   ",
            "empty": ""
        }"#,
    );
    let secrets = parse_bootstrap_provider_api_keys_b64(&encoded_map)
        .expect("formal provider-key environment entries should parse");

    assert_eq!(
        secrets,
        vec![
            UiBootstrapAiProviderSecret {
                provider_kind: "Zeta/Gateway".into(),
                api_key: "value$#\"\\юникод".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "alpha.7".into(),
                api_key: "alpha-secret".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "whitespace-only".into(),
                api_key: "   ".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "zeta-gateway".into(),
                api_key: "  zeta-secret  ".into(),
            },
            UiBootstrapAiProviderSecret {
                provider_kind: "zeta_gateway".into(),
                api_key: "underscore-secret".into(),
            },
        ]
    );
}

#[test]
fn bootstrap_provider_secret_json_rejects_invalid_kind_without_leaking_value() {
    let secret_value = "must-not-appear-in-diagnostics";
    let error =
        parse_bootstrap_provider_api_keys_json(&format!(r#"{{"invalid kind":"{secret_value}"}}"#))
            .expect_err("non-canonical provider suffix must fail closed");

    assert!(error.contains("decoded JSON is invalid"));
    assert!(!error.contains(secret_value));
}

#[test]
fn bootstrap_provider_secret_json_rejects_duplicate_kind_without_leaking_values() {
    let error = parse_bootstrap_provider_api_keys_json(
        r#"{"arbitrary-provider":"first-secret","arbitrary-provider":"second-secret"}"#,
    )
    .expect_err("conflicting duplicate provider credentials must fail closed");

    assert!(error.contains("decoded JSON is invalid"));
    assert!(!error.contains("first-secret"));
    assert!(!error.contains("second-secret"));
}

#[test]
fn empty_bootstrap_provider_secret_json_is_absent_without_alias_defaults() {
    let secrets = parse_bootstrap_provider_api_keys_b64("")
        .expect("an empty generic provider map should be accepted");

    assert!(secrets.is_empty());
}

#[test]
fn bootstrap_provider_secret_base64_rejects_surrounding_whitespace() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let encoded_map = STANDARD.encode(r#"{"arbitrary-provider":"secret"}"#);
    let error = parse_bootstrap_provider_api_keys_b64(&format!(" {encoded_map}"))
        .expect_err("canonical base64 must not be silently trimmed");

    assert!(error.contains("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!error.contains(&encoded_map));
}

#[test]
fn bootstrap_provider_secret_payload_limits_fail_closed_without_leaking_values() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let oversized_key = "x".repeat(MAX_BOOTSTRAP_PROVIDER_API_KEY_BYTES + 1);
    let oversized_key_json = serde_json::json!({ "arbitrary-provider": oversized_key }).to_string();
    let key_error = parse_bootstrap_provider_api_keys_json(&oversized_key_json)
        .expect_err("oversized credentials must fail closed");
    assert_eq!(key_error, "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 decoded JSON is invalid");

    let too_many_entries = format!(
        "{{{}}}",
        (0..=MAX_BOOTSTRAP_PROVIDER_SECRETS)
            .map(|index| format!(r#""provider-{index}":"value""#))
            .collect::<Vec<_>>()
            .join(",")
    );
    let entry_error = parse_bootstrap_provider_api_keys_json(&too_many_entries)
        .expect_err("too many provider credentials must fail closed");
    assert_eq!(entry_error, "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 decoded JSON is invalid");

    let oversized_document = vec![b'x'; MAX_BOOTSTRAP_PROVIDER_MAP_JSON_BYTES + 1];
    let encoded_document = STANDARD.encode(oversized_document);
    let document_error = parse_bootstrap_provider_api_keys_b64(&encoded_document)
        .expect_err("oversized decoded documents must fail before JSON parsing");
    assert!(document_error.contains("exceeds the configured size limit"));
    assert!(!document_error.contains(&encoded_document));
}

#[test]
fn bootstrap_provider_secret_base64_errors_never_include_encoded_value() {
    let encoded_value = "not-valid-base64$#";
    let error = parse_bootstrap_provider_api_keys_b64(encoded_value)
        .expect_err("malformed base64 must fail closed");

    assert!(error.contains("IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"));
    assert!(!error.contains(encoded_value));
}

#[test]
fn resolved_ui_bootstrap_ai_exposes_binding_defaults_without_provider_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_extract_text_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_extract_text_model_name = Some(" alpha-multimodal ".into());
    settings.ui_bootstrap_extract_graph_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_extract_graph_model_name = Some(" alpha-chat-small ".into());
    settings.ui_bootstrap_embed_chunk_provider_kind = Some(" provider-beta ".into());
    settings.ui_bootstrap_embed_chunk_model_name = Some(" beta-embedding-large ".into());
    settings.ui_bootstrap_query_compile_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_query_compile_model_name = Some(" alpha-chat-plus ".into());
    settings.ui_bootstrap_query_answer_provider_kind = Some(" provider-alpha ".into());
    settings.ui_bootstrap_query_answer_model_name = Some(" alpha-chat-large ".into());
    settings.ui_bootstrap_agent_provider_kind = Some(" provider-gamma ".into());
    settings.ui_bootstrap_agent_model_name = Some(" gamma-tools ".into());

    assert_eq!(
        settings.resolved_ui_bootstrap_ai_setup(),
        Some(UiBootstrapAiSetup {
            provider_secrets: vec![],
            binding_defaults: vec![
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::ExtractText,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-multimodal".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::ExtractGraph,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-small".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::EmbedChunk,
                    provider_kind: Some("provider-beta".into()),
                    model_name: Some("beta-embedding-large".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::QueryCompile,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-plus".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::QueryAnswer,
                    provider_kind: Some("provider-alpha".into()),
                    model_name: Some("alpha-chat-large".into()),
                },
                UiBootstrapAiBindingDefault {
                    binding_purpose: AiBindingPurpose::Agent,
                    provider_kind: Some("provider-gamma".into()),
                    model_name: Some("gamma-tools".into()),
                },
            ],
        }),
    );
}

#[test]
fn bootstrap_settings_expose_canonical_boundary() {
    let settings = sample_settings();
    let bootstrap = settings.bootstrap_settings();

    assert_eq!(bootstrap.ui_bootstrap_admin, None);
}

#[test]
fn bootstrap_settings_resolve_explicit_admin_credentials() {
    let mut settings = sample_settings();
    settings.ui_bootstrap_admin_login = Some(" root ".into());
    settings.ui_bootstrap_admin_password = Some(" secret ".into());

    let bootstrap = settings.bootstrap_settings();

    assert_eq!(
        bootstrap.ui_bootstrap_admin,
        Some(UiBootstrapAdmin {
            login: "root".into(),
            email: "root@ironrag.local".into(),
            display_name: "Admin".into(),
            password: "secret".into(),
            api_token: None,
        })
    );
}

#[test]
fn public_origin_settings_split_and_trim_allowed_origins() {
    let mut settings = sample_settings();
    settings.frontend_origin = " https://app.example.com , http://localhost:19000 ".into();

    let origins = settings.public_origin_settings();

    assert_eq!(origins.raw_frontend_origin, " https://app.example.com , http://localhost:19000 ");
    assert_eq!(
        origins.allowed_origins,
        vec!["https://app.example.com".to_string(), "http://localhost:19000".to_string()]
    );
    assert!(origins.session_cookie_secure);
}

#[test]
fn public_origin_settings_leave_local_http_session_cookies_non_secure() {
    let settings = sample_settings();

    let origins = settings.public_origin_settings();

    assert!(!origins.session_cookie_secure);
}

#[test]
fn mcp_http_origin_policy_accepts_frontend_and_canonical_public_origins() {
    let mut settings = sample_settings();
    settings.frontend_origin = " https://console.example , http://localhost:19000 ".into();
    settings.openapi_public_origin = Some("https://api.example".into());

    let policy = settings.mcp_http_origin_policy().expect("synthetic origin policy must be valid");

    assert!(policy.allows("https://console.example"));
    assert!(policy.allows("http://localhost:19000"));
    assert!(policy.allows("https://api.example"));
    assert!(!policy.allows("https://untrusted.example"));
    assert!(!policy.allows("https://console.example/"));
}

#[test]
fn mcp_http_origin_policy_rejects_noncanonical_configuration() {
    for invalid in [
        "not-an-origin",
        "https://console.example/path",
        "https://user@console.example",
        "https://console.example/",
        "ftp://console.example",
    ] {
        let mut settings = sample_settings();
        settings.frontend_origin = invalid.into();

        assert!(settings.mcp_http_origin_policy().is_err(), "accepted {invalid}");
    }

    let mut settings = sample_settings();
    settings.openapi_public_origin = Some("https://api.example/v1".into());
    assert!(settings.mcp_http_origin_policy().is_err());
}

#[test]
fn destructive_fresh_bootstrap_settings_default_to_disabled() {
    let settings = sample_settings();
    let destructive = settings.destructive_fresh_bootstrap_settings();

    assert!(!destructive.required);
}

#[test]
fn rejects_invalid_mcp_memory_ranges() {
    let mut settings = sample_settings();
    settings.mcp_memory_default_read_window_chars = 10_000;
    settings.mcp_memory_max_read_window_chars = 100;

    let error = validate_mcp_memory_settings(&settings).expect_err("range should fail");
    assert!(error.contains("mcp_memory_default_read_window_chars"));
}

#[test]
fn rejects_invalid_runtime_agent_limits() {
    let mut settings = sample_settings();
    settings.runtime_agent_max_turns = 0;

    let error =
        validate_runtime_agent_settings(&settings).expect_err("runtime settings should fail");
    assert!(error.contains("runtime_agent_max_turns"));
}

#[test]
fn service_role_helpers_match_role() {
    let mut settings = sample_settings();

    settings.service_role = "api".into();
    assert!(settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "worker".into();
    assert!(!settings.runs_http_api());
    assert!(settings.runs_probe_http_api());
    assert!(settings.runs_ingestion_workers());
    assert!(!settings.runs_startup_authority());

    settings.service_role = "startup".into();
    assert!(!settings.runs_http_api());
    assert!(!settings.runs_probe_http_api());
    assert!(!settings.runs_ingestion_workers());
    assert!(settings.runs_startup_authority());
}

#[test]
fn rejects_invalid_service_roles() {
    let mut settings = sample_settings();
    settings.service_role = "scheduler".into();

    let error = validate_service_role(&settings).expect_err("invalid role should fail");
    assert!(error.contains("service_role"));
}

#[test]
fn rejects_filesystem_cluster_topology() {
    let mut settings = sample_settings();
    settings.content_storage_topology = "shared_cluster".into();

    let error = validate_content_storage_settings(&settings).expect_err("shared cluster must fail");
    assert!(error.contains("content_storage_topology"));
}

#[test]
fn rejects_s3_provider_without_credentials() {
    let mut settings = sample_settings();
    settings.content_storage_provider = "s3".into();
    settings.dependency_object_storage_mode = "bundled".into();

    let error = validate_content_storage_settings(&settings).expect_err("s3 settings must fail");
    assert!(error.contains("content_storage_s3_bucket"));
}

#[test]
fn accepts_service_names_with_identity_safe_characters() {
    let mut settings = sample_settings();
    settings.service_name = "ironrag.worker_01-api".into();

    validate_service_name(&settings).expect("valid service name should pass");
}

#[test]
fn rejects_invalid_service_names() {
    let mut settings = sample_settings();
    settings.service_name = "worker:api".into();

    let error = validate_service_name(&settings).expect_err("invalid service name should fail");
    assert!(error.contains("service_name"));
}

#[test]
fn rejects_invalid_release_check_repository_slug() {
    let mut settings = sample_settings();
    settings.release_check_repository = "https://github.com/mlimarenko/IronRAG".into();

    let error = validate_release_monitor_settings(&settings)
        .expect_err("full urls should fail release repository validation");
    assert!(error.contains("release_check_repository"));
}

#[test]
fn rejects_zero_release_check_interval() {
    let mut settings = sample_settings();
    settings.release_check_interval_hours = 0;

    let error = validate_release_monitor_settings(&settings)
        .expect_err("zero interval should fail release monitor validation");
    assert!(error.contains("release_check_interval_hours"));
}

#[test]
fn rejects_zero_graph_gc_interval() {
    let mut settings = sample_settings();
    settings.graph_gc_hours = 0;

    let error =
        validate_graph_gc_settings(&settings).expect_err("zero interval should fail graph GC");
    assert!(error.contains("graph_gc_hours"));
}
