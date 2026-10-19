use super::*;

#[test]
fn ironrag_environment_source_ignores_double_underscore_compatibility_names() {
    let settings = settings_from_env_entries(&[("IRONRAG__BIND_ADDR", "127.0.0.1:9292")]);

    assert_eq!(settings.bind_addr, "0.0.0.0:8080");
}

#[test]
fn from_env_has_sane_local_defaults() {
    // Hermetic on purpose: Settings::from_env() reads the real process
    // environment, so any ambient IRONRAG_* variable (CI service wiring,
    // developer shells) would flip these default assertions.
    let settings = settings_from_env_entries(&[]);

    // Grouped by configuration section so each helper reads as the defaults
    // of one concern; every assertion below is unchanged.
    assert_local_runtime_identity_and_secrets(&settings);
    assert_ingestion_and_scheduler_defaults(&settings);
    assert_provider_resilience_defaults(&settings);
    assert_query_turn_admission_defaults(&settings);
    assert_runtime_agent_graph_and_release_defaults(&settings);
    assert_recognition_and_budget_defaults(&settings);
    assert_ui_and_query_rerank_defaults(&settings);
    assert_storage_and_graph_filter_defaults(&settings);
    assert_mcp_memory_defaults(&settings);
}

fn assert_local_runtime_identity_and_secrets(settings: &Settings) {
    assert_eq!(settings.bind_addr, "0.0.0.0:8080");
    assert_eq!(settings.service_role, "api");
    assert_eq!(settings.service_name, "ironrag-backend");
    assert_eq!(settings.environment, "local");
    assert_eq!(settings.database_max_connections, 20);
    assert_eq!(settings.api_replicas, 1);
    assert_eq!(settings.worker_replicas, 1);
    assert_eq!(settings.knowledge_plane_backend, "postgres");
    assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
    assert_eq!(settings.log_filter, "info");
    assert!(settings.credential_master_key.is_none());
    assert!(settings.credential_master_key_id.is_none());
    assert!(settings.credential_previous_master_keys.is_none());
}

fn assert_ingestion_and_scheduler_defaults(settings: &Settings) {
    assert_eq!(settings.ingestion_graph_extract_parallelism_per_doc, 16);
    assert_eq!(settings.ingestion_max_parallel_jobs_global, 64);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_workspace, 16);
    assert_eq!(settings.ingestion_max_parallel_jobs_per_library, 4);
    assert_eq!(settings.ingestion_memory_soft_limit_mib, 0);
}

fn assert_provider_resilience_defaults(settings: &Settings) {
    assert_eq!(settings.provider_concurrency_max_outbound, 16);
    assert_eq!(settings.provider_concurrency_query_reserved, 4);
    assert_eq!(settings.provider_concurrency_acquire_timeout_ms, 30_000);
    assert_eq!(settings.provider_concurrency_registry_max_entries, 64);
    assert_eq!(settings.provider_concurrency_registry_idle_ttl_seconds, 900);
    assert_eq!(settings.provider_rate_limit_requests_per_second, 8);
    assert_eq!(settings.provider_rate_limit_burst, 16);
    assert_eq!(settings.provider_retry_max_attempts, 3);
    assert_eq!(settings.provider_retry_initial_backoff_ms, 200);
    assert_eq!(settings.provider_retry_max_backoff_ms, 5_000);
}

fn assert_query_turn_admission_defaults(settings: &Settings) {
    assert_eq!(settings.query_turn_max_concurrent, 32);
    assert_eq!(settings.query_turn_admission_queue, 80);
    assert_eq!(settings.query_turn_admission_timeout_ms, 15_000);
}

fn assert_runtime_agent_graph_and_release_defaults(settings: &Settings) {
    assert_eq!(settings.runtime_agent_max_turns, 4);
    assert_eq!(settings.runtime_agent_max_parallel_actions, 4);
    assert_eq!(settings.runtime_graph_extract_idle_timeout_seconds, 300);
    assert!(settings.runtime_graph_extract_recovery_enabled);
    assert_eq!(settings.release_check_repository, "mlimarenko/IronRAG");
    assert_eq!(settings.release_check_interval_hours, 12);
    assert_eq!(settings.graph_gc_hours, 24);
}

fn assert_recognition_and_budget_defaults(settings: &Settings) {
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
}

fn assert_ui_and_query_rerank_defaults(settings: &Settings) {
    // Browser session policy: three days of inactivity, thirty days absolute.
    // Both are configuration, so a deployment can tighten or relax them
    // without a code change.
    assert_eq!(settings.ui_session_idle_ttl_hours, 72);
    assert_eq!(settings.ui_session_absolute_ttl_hours, 720);
    assert_eq!(settings.query_intent_cache_ttl_hours, 24);
    assert!(settings.query_rerank_enabled);
    assert_eq!(settings.query_semantic_rerank_mode, crate::domains::query::SemanticRerankMode::Off);
    assert_eq!(
        settings.query_semantic_rerank_timeout_ms,
        crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS
    );
    assert_eq!(settings.query_semantic_rerank_candidate_limit, 16);
    assert_eq!(settings.query_semantic_rerank_candidate_text_chars, 1_200);
    assert_eq!(settings.query_semantic_rerank_total_text_chars, 18_000);
}

fn assert_storage_and_graph_filter_defaults(settings: &Settings) {
    assert_eq!(settings.content_storage_root, "/var/lib/ironrag/content-storage");
    assert_eq!(settings.runtime_document_activity_freshness_seconds, 90);
    assert_eq!(settings.runtime_document_stalled_after_seconds, 240);
    assert!(settings.runtime_graph_filter_empty_relations);
    assert!(settings.runtime_graph_filter_degenerate_self_loops);
    assert_eq!(settings.runtime_graph_convergence_warning_backlog_threshold, 1);
    assert!(!settings.runtime_graph_projection_prewarm_enabled);
    assert_eq!(settings.runtime_graph_projection_prewarm_max_libraries, 0);
}

fn assert_mcp_memory_defaults(settings: &Settings) {
    assert_eq!(settings.mcp_memory_default_read_window_chars, 48_000);
    assert_eq!(settings.mcp_memory_max_read_window_chars, 192_000);
    assert_eq!(settings.mcp_memory_default_search_limit, 10);
    assert_eq!(settings.mcp_memory_max_search_limit, 25);
    assert_eq!(settings.mcp_memory_idempotency_retention_hours, 72);
    assert!(settings.mcp_memory_audit_enabled);
}

#[test]
fn blank_oidc_environment_entries_are_treated_as_disabled() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_OIDC_ISSUER_URL", " "),
        ("IRONRAG_OIDC_CLIENT_ID", ""),
        ("IRONRAG_OIDC_CLIENT_SECRET", "\t"),
        ("IRONRAG_OIDC_REDIRECT_URL", ""),
        ("IRONRAG_OIDC_REQUIRED_GROUP", "\n"),
        ("IRONRAG_OIDC_BOOTSTRAP_ADMIN_SUBJECT", " "),
    ]);

    assert_eq!(settings.resolved_oidc_settings(), Ok(None));
}

#[test]
fn provider_resilience_validation_rejects_invalid_bounds() {
    let mut settings = sample_settings();
    settings.provider_concurrency_max_outbound = 4;
    settings.provider_concurrency_query_reserved = 4;
    assert!(validate_provider_resilience_settings(&settings).is_err());

    settings.provider_concurrency_max_outbound = 0;
    settings.provider_concurrency_query_reserved = 1;
    assert!(validate_provider_resilience_settings(&settings).is_err());

    settings.provider_concurrency_max_outbound = 4;
    settings.provider_concurrency_query_reserved = 1;
    settings.provider_concurrency_registry_max_entries = 0;
    assert!(validate_provider_resilience_settings(&settings).is_err());

    settings.provider_concurrency_registry_max_entries = 64;
    settings.provider_rate_limit_requests_per_second = 0;
    assert!(validate_provider_resilience_settings(&settings).is_err());

    settings.provider_rate_limit_burst = 0;
    settings.provider_retry_max_attempts = 0;
    assert!(validate_provider_resilience_settings(&settings).is_err());

    settings.provider_retry_max_attempts = 3;
    settings.provider_retry_initial_backoff_ms = 10_000;
    assert!(validate_provider_resilience_settings(&settings).is_err());
}

#[test]
fn provider_resilience_environment_overrides_are_typed() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND", "12"),
        ("IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED", "3"),
        ("IRONRAG_PROVIDER_CONCURRENCY_ACQUIRE_TIMEOUT_MS", "5000"),
        ("IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_MAX_ENTRIES", "32"),
        ("IRONRAG_PROVIDER_CONCURRENCY_REGISTRY_IDLE_TTL_SECONDS", "120"),
        ("IRONRAG_PROVIDER_RATE_LIMIT_REQUESTS_PER_SECOND", "25"),
        ("IRONRAG_PROVIDER_RATE_LIMIT_BURST", "50"),
        ("IRONRAG_PROVIDER_RETRY_MAX_ATTEMPTS", "4"),
        ("IRONRAG_PROVIDER_RETRY_INITIAL_BACKOFF_MS", "125"),
        ("IRONRAG_PROVIDER_RETRY_MAX_BACKOFF_MS", "4000"),
    ]);

    assert_eq!(settings.provider_concurrency_max_outbound, 12);
    assert_eq!(settings.provider_concurrency_query_reserved, 3);
    assert_eq!(settings.provider_concurrency_acquire_timeout_ms, 5_000);
    assert_eq!(settings.provider_concurrency_registry_max_entries, 32);
    assert_eq!(settings.provider_concurrency_registry_idle_ttl_seconds, 120);
    assert_eq!(settings.provider_rate_limit_requests_per_second, 25);
    assert_eq!(settings.provider_rate_limit_burst, 50);
    assert_eq!(settings.provider_retry_max_attempts, 4);
    assert_eq!(settings.provider_retry_initial_backoff_ms, 125);
    assert_eq!(settings.provider_retry_max_backoff_ms, 4_000);
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
fn credential_keyring_env_is_opt_in_bounded_and_strictly_normalized() {
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
    validate_credential_master_key(&settings).expect("identity keyring should validate");

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
fn credential_master_key_is_optional_at_the_configuration_boundary() {
    // A fully absent keyring loads: the mandatory cipher is enforced later when
    // AppState builds it, not at settings load, so keyless admin/maintenance
    // loads still succeed.
    let mut absent = sample_settings();
    absent.credential_master_key = None;
    absent.credential_master_key_id = None;
    absent.credential_previous_master_keys = None;
    validate_credential_master_key(&absent).expect("absent keyring is accepted at load");
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
fn default_semantic_rerank_timeout_passes_the_active_turn_wait_gate() {
    let mut settings = settings_from_env_entries(&[]);
    assert!(
        settings.query_semantic_rerank_timeout_ms
            <= crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS,
        "the default provider budget must fit the turn wait budget"
    );
    settings.query_semantic_rerank_mode = crate::domains::query::SemanticRerankMode::Active;

    validate_query_rerank_settings(&settings)
        .expect("the default active rerank must fit the turn wait budget");
}

#[test]
fn active_semantic_rerank_timeout_above_the_turn_wait_budget_is_rejected() {
    let mut settings = settings_from_env_entries(&[]);
    settings.query_semantic_rerank_mode = crate::domains::query::SemanticRerankMode::Active;
    settings.query_semantic_rerank_timeout_ms = crate::app::state::SEMANTIC_RERANK_TURN_WAIT_MS + 1;

    let error = validate_query_rerank_settings(&settings)
        .expect_err("an active rerank budget above the turn wait must fail closed");

    assert!(error.contains("query_semantic_rerank_timeout_ms"));
    assert!(error.contains("turn wait budget"));
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
fn ironrag_prefixed_flat_variables_override_defaults() {
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
fn query_turn_admission_env_overrides_defaults() {
    let settings = settings_from_env_entries(&[
        ("IRONRAG_QUERY_TURN_MAX_CONCURRENT", "40"),
        ("IRONRAG_QUERY_TURN_ADMISSION_QUEUE", "60"),
        ("IRONRAG_QUERY_TURN_ADMISSION_TIMEOUT_MS", "8000"),
    ]);

    assert_eq!(settings.query_turn_max_concurrent, 40);
    assert_eq!(settings.query_turn_admission_queue, 60);
    assert_eq!(settings.query_turn_admission_timeout_ms, 8_000);
}

#[test]
fn query_turn_admission_rejects_zero_concurrency() {
    let mut settings = sample_settings();
    settings.query_turn_max_concurrent = 0;

    assert_eq!(
        validate_query_turn_admission_settings(&settings),
        Err("query_turn_max_concurrent must be greater than zero".into()),
    );
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
fn ingestion_limit_variables_override_defaults() {
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
fn graph_gc_interval_variable_overrides_default() {
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
fn graph_extract_parallelism_is_clamped_to_the_ingest_lane_waiting_bound() {
    assert_eq!(resolve_ingestion_graph_extract_parallelism(16, 2, 16, 4), Ok(12),);
    assert_eq!(resolve_ingestion_graph_extract_parallelism(16, 4, 16, 4), Ok(6),);
    assert_eq!(resolve_ingestion_graph_extract_parallelism(4, 2, 16, 4), Ok(4),);
    assert_eq!(resolve_ingestion_graph_extract_parallelism(16, 64, 0, 0), Ok(16),);
}

#[test]
fn graph_extract_parallelism_rejects_an_unclampable_job_ceiling() {
    let error = resolve_ingestion_graph_extract_parallelism(16, 25, 16, 4)
        .expect_err("one provider call per job would exceed the waiting bound");

    assert!(error.contains("ingestion_max_parallel_jobs_global"));
    assert!(error.contains("provider_concurrency_max_outbound"));
}
